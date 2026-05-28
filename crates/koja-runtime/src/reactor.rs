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

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::BorrowedFd;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use polling::{Event, Events, PollMode, Poller};

use crate::ffi::{EAGAIN, get_errno, koja_context_switch};
use crate::scheduler::{
    CURRENT_PID, IO_READY_ERROR, IO_READY_READ, IO_READY_WRITE, ProcessState, SCHED, SCHED_SP,
    SHUTDOWN, WORK_AVAILABLE, YIELD_SP, send_io_event,
};

/// Whether the reactor should wake for readable or writable readiness.
#[derive(Clone, Copy)]
pub enum Interest {
    Readable,
    Writable,
}

/// Key offset for watched fds so they don't collide with io_block keys
/// (which use the PID, starting at 1).
const WATCH_KEY_OFFSET: usize = 1_000_000;

/// Global I/O reactor. Initialized once at startup.
///
/// The `Poller` is thread-safe internally. The `registered` set tracks
/// which fds are currently in the poller so we know whether to `add` or
/// `modify` on re-registration.
///
/// `watched` maps event keys (fd + WATCH_KEY_OFFSET) to (owner_pid, fd)
/// for fds registered via `Fd.watch`. When the reactor fires an event
/// for a watched key, it sends an `IOReady` message instead of marking
/// the process Runnable.
struct Reactor {
    poller: Poller,
    registered: Mutex<HashSet<i32>>,
    watched: Mutex<HashMap<usize, (i64, i32)>>,
}

/// Singleton reactor instance, created in [`init`].
static REACTOR: OnceLock<Reactor> = OnceLock::new();

/// Initializes the global reactor. Called once from `koja_rt_main_done`
/// before spawning the reactor thread.
pub fn init() {
    REACTOR.get_or_init(|| Reactor {
        poller: Poller::new().expect("failed to create I/O poller"),
        registered: Mutex::new(HashSet::new()),
        watched: Mutex::new(HashMap::new()),
    });
}

/// Adds or modifies a file descriptor in the poller with oneshot mode.
/// Handles the add-vs-modify distinction using the `registered` set.
fn poller_add_or_modify(fd: i32, event: Event) {
    let reactor = REACTOR.get().expect("reactor not initialized");
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

/// Registers a file descriptor for readiness notification.
///
/// Uses oneshot mode: after one event fires the fd stops generating
/// events until re-registered. The `key` is the process PID so the
/// reactor thread knows which process to wake.
fn register(fd: i32, interest: Interest, pid: i64) {
    let event = match interest {
        Interest::Readable => Event::readable(pid as usize),
        Interest::Writable => Event::writable(pid as usize),
    };
    poller_add_or_modify(fd, event);
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

        let mut io_events: Vec<(i64, u8, i64)> = Vec::new();

        {
            let watched_guard = reactor.watched.lock().unwrap();
            let mut sched_guard = SCHED.lock().unwrap();

            for ev in events.iter() {
                if let Some(&(owner_pid, fd)) = watched_guard.get(&ev.key) {
                    let variant: u8 = if ev.readable {
                        IO_READY_READ
                    } else if ev.writable {
                        IO_READY_WRITE
                    } else {
                        IO_READY_ERROR
                    };
                    io_events.push((owner_pid, variant, fd as i64));
                } else {
                    let pid = ev.key as i64;
                    let idx = (pid - 1) as usize;
                    if idx < sched_guard.processes.len()
                        && sched_guard.processes[idx].state == ProcessState::WaitingIo
                    {
                        sched_guard.processes[idx].state = ProcessState::Runnable;
                    }
                }
            }
        }

        for (pid, variant, fd) in io_events {
            send_io_event(pid, variant, fd);
        }

        WORK_AVAILABLE.notify_all();
    }
}

/// Registers a file descriptor for readiness monitoring. Instead of
/// blocking the process, readiness events are delivered as `IOReady`
/// messages to the process's mailbox (tag=2).
///
/// `interest`: 0 = Read, 1 = Write.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_watch_fd(fd: i32, interest: i64) {
    let reactor = REACTOR.get().expect("reactor not initialized");
    let pid = CURRENT_PID.with(|c| c.get());
    let key = WATCH_KEY_OFFSET + fd as usize;

    let event = if interest == 1 {
        Event::writable(key)
    } else {
        Event::readable(key)
    };

    poller_add_or_modify(fd, event);
    reactor.watched.lock().unwrap().insert(key, (pid, fd));
}

/// Removes a file descriptor from I/O readiness monitoring.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_unwatch_fd(fd: i32) {
    let reactor = REACTOR.get().expect("reactor not initialized");
    let key = WATCH_KEY_OFFSET + fd as usize;

    reactor.watched.lock().unwrap().remove(&key);

    let mut reg = reactor.registered.lock().unwrap();
    if reg.remove(&fd) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = reactor.poller.delete(borrowed);
    }
}

/// Drops `fd` from the reactor's `registered` / `watched` maps so
/// fd-number reuse can't collide with stale entries. Idempotent.
/// Does not wake any process currently `WaitingIo` on `fd`, so
/// close-while-blocked from another worker will strand that worker.
pub(crate) fn release_fd(fd: i32) {
    let Some(reactor) = REACTOR.get() else {
        return;
    };
    let key = WATCH_KEY_OFFSET + fd as usize;
    reactor.watched.lock().unwrap().remove(&key);

    let mut reg = reactor.registered.lock().unwrap();
    if reg.remove(&fd) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = reactor.poller.delete(borrowed);
    }
}

/// Suspends the current process until `fd` is ready for the given
/// [`Interest`]. Called from runtime I/O paths on `EAGAIN`.
///
/// State must be set to `WaitingIo` **before** `register`: the
/// reactor's wake guard checks `state == WaitingIo`, so a state of
/// `Running` at fire time silently drops the event and the process
/// parks forever. Reverse order means at worst a spurious resume.
pub fn io_block(fd: i32, interest: Interest) {
    let pid = CURRENT_PID.with(|c| c.get());
    let idx = (pid - 1) as usize;

    {
        let mut guard = SCHED.lock().unwrap();
        if idx < guard.processes.len() {
            guard.processes[idx].state = ProcessState::WaitingIo;
        }
    }

    register(fd, interest, pid);

    let yield_sp_ptr = YIELD_SP.with(|c| c.get());
    let sched_sp = unsafe { *SCHED_SP.with(|c| c.get()) };
    unsafe {
        koja_context_switch(yield_sp_ptr, sched_sp);
    }

    deregister(fd);
}

/// Runs a non-blocking syscall, suspending the process on `EAGAIN`
/// until `fd` is ready for `interest` and then retrying. Returns the
/// syscall's non-negative result, or the OS error captured at the
/// point of a non-`EAGAIN` failure. Callers own success handling and
/// error reporting (e.g. `set_last_error`).
pub(crate) fn block_until_ready(
    fd: i32,
    interest: Interest,
    mut syscall: impl FnMut() -> isize,
) -> io::Result<isize> {
    loop {
        let n = syscall();
        if n >= 0 {
            return Ok(n);
        }
        if get_errno() == EAGAIN {
            io_block(fd, interest);
            continue;
        }
        return Err(io::Error::last_os_error());
    }
}
