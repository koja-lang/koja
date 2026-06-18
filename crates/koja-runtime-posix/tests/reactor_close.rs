//! Close-while-blocked liveness harness.
//!
//! Reproduces the scenario the reactor's `release_fd` reverse-map fix
//! exists for: a process parked in `koja_fd_read` (`io_block`-ed on a
//! socket fd) while *another* process closes that same fd. The poller
//! keys waiters by pid, so without the fd->pid `blocking` map the closer
//! can't find the parked reader and it strands forever.
//!
//! Like `scheduler_stress`, this drives the real scheduler + reactor via
//! the runtime's `#[no_mangle]` C surface, with process bodies written as
//! plain `extern "C"` functions. The reader announces itself, blocks on a
//! never-readable socket, and is expected to wake with an error (the
//! retried read hits `EBADF` on the closed fd). The controller closes the
//! fd and waits for the reader's completion message; a regression makes
//! the reader hang, which the watchdog turns into a hard abort so the
//! suite fails loudly instead of stalling.
//!
//! As with `scheduler_stress`, the runtime is a process-global singleton,
//! so this file contains exactly one `#[test]`.

use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

extern crate koja_runtime;

unsafe extern "C" {
    fn koja_rt_spawn(
        fn_ptr: extern "C" fn(*const u8),
        state_ptr: *const u8,
        state_len: i64,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    ) -> i64;
    fn koja_rt_send(
        pid: i64,
        msg_ptr: *const u8,
        msg_len: i64,
        drop_glue: Option<unsafe extern "C" fn(*mut u8)>,
    );
    fn koja_rt_receive(out: *mut u8, out_cap: i64) -> i64;
    fn koja_rt_self() -> i64;
    fn koja_rt_main_done();
    fn koja_fd_read(fd: i32, count: i64) -> *const u8;
    fn koja_fd_close(fd: i32) -> i32;
}

/// See `scheduler_stress`: the runtime's panic handler links against this.
#[unsafe(no_mangle)]
static __koja_app_name: [u8; 1] = [0];

/// PID the reader reports back to, published before the reader is spawned.
static CONTROLLER_PID: AtomicI64 = AtomicI64::new(0);
/// Raw fd the reader blocks on; closed out from under it by the controller.
static READER_FD: AtomicI64 = AtomicI64::new(-1);
/// Set by the reader when its blocked read returns an error (woke correctly).
static READER_ERRORED: AtomicBool = AtomicBool::new(false);
/// Set once the test's assertions are reached, disarming the watchdog.
static FINISHED: AtomicBool = AtomicBool::new(false);

const ABOUT_TO_BLOCK: u8 = 0xA1;
const DONE: u8 = 0xD0;

/// Blocks until a real message arrives, ignoring spurious empty wakes
/// (`koja_rt_receive` returns -1 when woken with an empty mailbox).
fn recv_blocking() {
    let mut byte = 0u8;
    while unsafe { koja_rt_receive(&mut byte, 1) } < 0 {}
}

/// Builds a connected localhost TCP pair, hands back the (non-blocking)
/// client fd, and leaks all three endpoints so std never closes them —
/// the test owns the fd's lifetime via `koja_fd_close`. Nothing is ever
/// written to the peer, so reading the client end always yields `EAGAIN`.
fn never_readable_fd() -> i32 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let client = TcpStream::connect(addr).expect("connect loopback");
    let (server, _) = listener.accept().expect("accept loopback");
    client.set_nonblocking(true).expect("set_nonblocking");

    let fd = client.as_raw_fd();
    std::mem::forget(listener);
    std::mem::forget(server);
    std::mem::forget(client);
    fd
}

/// Reader process body: announce readiness, block on a never-readable fd,
/// and report back. The read is expected to fail (`EBADF`) once the
/// controller closes the fd, returning a null string pointer.
extern "C" fn reader_entry(_state: *const u8) {
    let fd = READER_FD.load(Ordering::SeqCst) as i32;
    let controller = CONTROLLER_PID.load(Ordering::SeqCst);

    unsafe { koja_rt_send(controller, &ABOUT_TO_BLOCK, 1, None) };
    let result = unsafe { koja_fd_read(fd, 16) };
    READER_ERRORED.store(result.is_null(), Ordering::SeqCst);
    unsafe { koja_rt_send(controller, &DONE, 1, None) };
}

/// Controller (PID 1): spawn the reader, wait until it's about to block,
/// close the fd out from under it, and wait for its completion message.
extern "C" fn controller_entry(_state: *const u8) {
    CONTROLLER_PID.store(unsafe { koja_rt_self() }, Ordering::SeqCst);
    READER_FD.store(never_readable_fd() as i64, Ordering::SeqCst);

    unsafe { koja_rt_spawn(reader_entry, std::ptr::null(), 0, None) };

    // First message: the reader is about to enter its blocking read. The
    // short pause lets it actually park in `io_block` (register in the
    // reactor's `blocking` map) before we close, so we exercise the
    // close-*while-blocked* path rather than a pre-close fast `EBADF`.
    recv_blocking();
    std::thread::sleep(Duration::from_millis(50));

    let fd = READER_FD.load(Ordering::SeqCst) as i32;
    unsafe { koja_fd_close(fd) };

    // Second message: the reader's read returned. A regression strands it
    // here forever, which the watchdog converts into an abort.
    recv_blocking();
}

#[test]
fn close_wakes_blocked_reader() {
    std::thread::spawn(|| {
        for _ in 0..100 {
            if FINISHED.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("close_wakes_blocked_reader: blocked reader never woke (reactor regression)");
        std::process::abort();
    });

    unsafe {
        koja_rt_spawn(controller_entry, std::ptr::null(), 0, None);
        koja_rt_main_done();
    }

    FINISHED.store(true, Ordering::SeqCst);
    assert!(
        READER_ERRORED.load(Ordering::SeqCst),
        "reader blocked in koja_fd_read should wake with an error when its fd is closed",
    );
}
