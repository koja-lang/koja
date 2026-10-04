//! Socket deadline harness.
//!
//! Drives the real scheduler + reactor through the runtime's `#[no_mangle]`
//! C surface and checks that a bounded wait ends with `TimedOut` (cause
//! code 18) instead of parking forever. Three waits are covered: a read on
//! a peer that never writes, an accept on a listener no client reaches,
//! and a connect to a TEST-NET address (RFC 5737) that no router forwards,
//! so the SYN goes unanswered. A host with no route at all fails that
//! connect at once with an unreachable error, which the test accepts.
//!
//! As with `scheduler_stress`, the runtime is a process-global singleton,
//! so this file contains exactly one `#[test]`.

mod common;

use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use common::{koja_rt_main_done, spawn_simple};

extern crate koja_runtime;

unsafe extern "C" {
    fn koja_fd_read(fd: i32, count: i64, timeout_ms: i64) -> *const u8;
    fn koja_socket_accept(fd: i32, timeout_ms: i64) -> i32;
    fn koja_socket_connect(
        fd: i32,
        ip_ptr: *const u8,
        ip_length: i64,
        port: i64,
        timeout_ms: i64,
    ) -> i64;
    fn koja_socket_create(sock_type: i64) -> i32;
    fn koja_last_error_code() -> i32;
}

const SOCK_STREAM: i64 = 1;
/// Cause codes from `error_kind_code` in the runtime.
const HOST_UNREACHABLE: i64 = 9;
const NETWORK_UNREACHABLE: i64 = 14;
const TIMED_OUT: i64 = 18;
const TIMEOUT: Duration = Duration::from_millis(100);
/// 192.0.2.0/24 is reserved for documentation and never routed.
const TEST_NET_IP: [u8; 4] = [192, 0, 2, 1];

static READ_CODE: AtomicI64 = AtomicI64::new(-1);
static READ_ELAPSED_MS: AtomicI64 = AtomicI64::new(-1);
static ACCEPT_CODE: AtomicI64 = AtomicI64::new(-1);
static CONNECT_CODE: AtomicI64 = AtomicI64::new(-1);
static CONNECT_ELAPSED_MS: AtomicI64 = AtomicI64::new(-1);
static FINISHED: AtomicBool = AtomicBool::new(false);

/// A connected loopback client whose peer never writes. All endpoints
/// leak so std never closes them under the runtime.
fn silent_peer_fd() -> i32 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    client.set_nonblocking(true).expect("set_nonblocking");
    let fd = client.as_raw_fd();
    std::mem::forget(listener);
    std::mem::forget(server);
    std::mem::forget(client);
    fd
}

/// A non-blocking loopback listener that no client connects to.
fn idle_listener_fd() -> i32 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("set_nonblocking");
    let fd = listener.as_raw_fd();
    std::mem::forget(listener);
    fd
}

extern "C" fn body(_state: *const u8) {
    let started = Instant::now();
    let read = unsafe { koja_fd_read(silent_peer_fd(), 16, TIMEOUT.as_millis() as i64) };
    READ_ELAPSED_MS.store(started.elapsed().as_millis() as i64, Ordering::SeqCst);
    if read.is_null() {
        READ_CODE.store(
            i64::from(unsafe { koja_last_error_code() }),
            Ordering::SeqCst,
        );
    }

    let accepted = unsafe { koja_socket_accept(idle_listener_fd(), TIMEOUT.as_millis() as i64) };
    if accepted < 0 {
        ACCEPT_CODE.store(
            i64::from(unsafe { koja_last_error_code() }),
            Ordering::SeqCst,
        );
    }

    let fd = unsafe { koja_socket_create(SOCK_STREAM) };
    let started = Instant::now();
    let connected = unsafe {
        koja_socket_connect(
            fd,
            TEST_NET_IP.as_ptr(),
            TEST_NET_IP.len() as i64,
            9,
            TIMEOUT.as_millis() as i64,
        )
    };
    CONNECT_ELAPSED_MS.store(started.elapsed().as_millis() as i64, Ordering::SeqCst);
    if connected < 0 {
        CONNECT_CODE.store(
            i64::from(unsafe { koja_last_error_code() }),
            Ordering::SeqCst,
        );
    }
}

#[test]
fn bounded_waits_time_out() {
    std::thread::spawn(|| {
        for _ in 0..100 {
            if FINISHED.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("bounded_waits_time_out: a bounded wait never returned (deadline regression)");
        std::process::abort();
    });

    unsafe {
        spawn_simple(body);
        koja_rt_main_done();
    }
    FINISHED.store(true, Ordering::SeqCst);

    let timeout_ms = TIMEOUT.as_millis() as i64;
    assert_eq!(READ_CODE.load(Ordering::SeqCst), TIMED_OUT, "read");
    assert!(
        READ_ELAPSED_MS.load(Ordering::SeqCst) >= timeout_ms,
        "read returned before its deadline",
    );
    assert_eq!(ACCEPT_CODE.load(Ordering::SeqCst), TIMED_OUT, "accept");

    let connect_code = CONNECT_CODE.load(Ordering::SeqCst);
    let connect_elapsed = CONNECT_ELAPSED_MS.load(Ordering::SeqCst);
    match connect_code {
        TIMED_OUT => assert!(
            connect_elapsed >= timeout_ms,
            "connect returned before its deadline",
        ),
        HOST_UNREACHABLE | NETWORK_UNREACHABLE => {}
        other => panic!("connect: unexpected cause code {other} after {connect_elapsed} ms"),
    }
}
