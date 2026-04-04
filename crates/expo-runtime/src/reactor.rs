//! I/O reactor backed by the `polling` crate (kqueue on macOS, epoll on Linux).
//!
//! A single dedicated reactor thread runs [`reactor_loop`], polling for
//! readiness events on registered file descriptors. When a fd becomes
//! ready, the reactor promotes the associated process from `WaitingIo`
//! to `Runnable` and wakes a worker thread via the scheduler's Condvar.
//!
//! I/O-performing runtime functions (accept, read, write, etc.) call
//! [`io_block`] when a syscall returns `EAGAIN`. This registers the fd,
//! marks the process `WaitingIo`, and context-switches back to the
//! scheduler. When the reactor detects readiness, the process resumes
//! and retries the syscall.

use std::collections::HashSet;
use std::io;
use std::os::fd::BorrowedFd;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use polling::{Event, Events, PollMode, Poller};

use crate::ffi::expo_context_switch;
use crate::scheduler::{
    CURRENT_PID, ProcessState, SCHED, SCHED_SP, SHUTDOWN, WORK_AVAILABLE, YIELD_SP,
};

/// Whether the reactor should wake for readable or writable readiness.
#[derive(Clone, Copy)]
pub enum Interest {
    Readable,
    Writable,
}

/// Global I/O reactor. Initialized once at startup.
///
/// The `Poller` is thread-safe internally. The `registered` set tracks
/// which fds are currently in the poller so we know whether to `add` or
/// `modify` on re-registration.
struct Reactor {
    poller: Poller,
    registered: Mutex<HashSet<i32>>,
}

/// Singleton reactor instance, created in [`init`].
static REACTOR: OnceLock<Reactor> = OnceLock::new();

/// Initializes the global reactor. Called once from `expo_rt_main_done`
/// before spawning the reactor thread.
pub fn init() {
    REACTOR.get_or_init(|| Reactor {
        poller: Poller::new().expect("failed to create I/O poller"),
        registered: Mutex::new(HashSet::new()),
    });
}

/// Registers a file descriptor for readiness notification.
///
/// Uses oneshot mode: after one event fires the fd stops generating
/// events until re-registered. The `key` is the process PID so the
/// reactor thread knows which process to wake.
fn register(fd: i32, interest: Interest, pid: i64) {
    let reactor = REACTOR.get().expect("reactor not initialized");
    let event = match interest {
        Interest::Readable => Event::readable(pid as usize),
        Interest::Writable => Event::writable(pid as usize),
    };
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut set = reactor.registered.lock().unwrap();

    if set.contains(&fd) {
        let _ = reactor
            .poller
            .modify_with_mode(borrowed, event, PollMode::Oneshot);
    } else {
        unsafe {
            let _ = reactor
                .poller
                .add_with_mode(&borrowed, event, PollMode::Oneshot);
        }
        set.insert(fd);
    }
}

/// Removes a file descriptor from the reactor.
fn deregister(fd: i32) {
    let reactor = REACTOR.get().expect("reactor not initialized");
    let mut set = reactor.registered.lock().unwrap();
    if set.remove(&fd) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = reactor.poller.delete(borrowed);
    }
}

/// Wakes the reactor thread from its `poller.wait()` call.
/// Used during shutdown to unblock the reactor so it can exit.
pub fn notify() {
    if let Some(reactor) = REACTOR.get() {
        let _ = reactor.poller.notify();
    }
}

/// Dedicated reactor thread loop.
///
/// Polls for I/O readiness events and promotes waiting processes back
/// to `Runnable`. Exits when the global [`SHUTDOWN`] flag is set.
pub fn reactor_loop() {
    let reactor = REACTOR.get().expect("reactor not initialized");
    let mut events = Events::new();

    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }

        events.clear();
        let timeout = Some(Duration::from_millis(50));
        match reactor.poller.wait(&mut events, timeout) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }

        if events.is_empty() {
            continue;
        }

        let mut guard = SCHED.lock().unwrap();
        for ev in events.iter() {
            let pid = ev.key as i64;
            let idx = (pid - 1) as usize;
            if idx < guard.processes.len() && guard.processes[idx].state == ProcessState::WaitingIo
            {
                guard.processes[idx].state = ProcessState::Runnable;
            }
        }
        drop(guard);
        WORK_AVAILABLE.notify_all();
    }
}

/// Suspends the current process until `fd` is ready for the given
/// [`Interest`].
///
/// Called by runtime I/O functions when a syscall returns `EAGAIN`.
/// Registers the fd with the reactor, sets the process to `WaitingIo`,
/// and context-switches back to the scheduler. Returns when a worker
/// resumes this process after the reactor detects readiness.
pub fn io_block(fd: i32, interest: Interest) {
    let pid = CURRENT_PID.with(|c| c.get());
    let idx = (pid - 1) as usize;

    register(fd, interest, pid);

    {
        let mut guard = SCHED.lock().unwrap();
        if idx < guard.processes.len() {
            guard.processes[idx].state = ProcessState::WaitingIo;
        }
    }

    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        expo_context_switch(yield_sp_ptr, sched_sp);
    }

    deregister(fd);
}
