//! I/O reactor backed by the `polling` crate (kqueue on macOS, epoll on Linux).
//!
//! A single dedicated reactor thread runs [`reactor_loop`], driving the
//! [`NativeReactor`]'s [`poll`](Reactor::poll) and applying the [`Waker`]s
//! it returns: promoting a process blocked on a fd from `WaitingIO` to
//! `Runnable`, or delivering an `IOReady` message to a `Fd.watch` owner.
//!
//! I/O-performing runtime functions (accept, read, write, etc.) call
//! [`io_block`] when a syscall returns `EAGAIN`. This registers the fd as
//! a [`Waker::Resume`], marks the process `WaitingIO`, and context-switches
//! back to the scheduler. When the reactor reports readiness, the process
//! resumes and retries the syscall.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::BorrowedFd;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use polling::{Event, Events, PollMode, Poller};

use koja_runtime_core::{ProcessState, Reactor, Readiness, Waker};

use crate::ffi::{EAGAIN, EINTR, get_errno};
use crate::scheduler::{
    CURRENT_PID, NativeTable, SCHED, SHUTDOWN, WORK_AVAILABLE, publish_ready, send_io_event,
    yield_to_scheduler,
};
use crate::wire::{IO_READY_ERROR, IO_READY_READ, IO_READY_WRITE};

pub use koja_runtime_core::Interest;

/// The native [`Reactor`]: a `polling` poller plus the bookkeeping to map
/// a fired event back to the [`Waker`] registered for its fd.
///
/// The poller tracks exactly one registration per fd, so a single
/// `fd -> Waker` map is the faithful model: the last `register` for an fd
/// wins, matching the poller's own semantics. `registered` mirrors which
/// fds are currently armed so we know whether to `add` or `modify`.
struct NativeReactor {
    poller: Poller,
    registered: Mutex<HashSet<i32>>,
    wakers: Mutex<HashMap<i32, Waker>>,
}

impl NativeReactor {
    /// Adds or modifies `fd` in the poller with oneshot mode, using the
    /// `registered` set to pick `add` vs `modify`.
    fn arm(&self, fd: i32, event: Event) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let mut set = self.registered.lock().unwrap();
        if set.contains(&fd) {
            let _ = self
                .poller
                .modify_with_mode(borrowed, event, PollMode::Oneshot);
        } else {
            unsafe {
                let _ = self
                    .poller
                    .add_with_mode(&borrowed, event, PollMode::Oneshot);
            }
            set.insert(fd);
        }
    }

    /// Drops `fd` from the poller and the `registered` set. Idempotent.
    fn disarm(&self, fd: i32) {
        let mut set = self.registered.lock().unwrap();
        if set.remove(&fd) {
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            let _ = self.poller.delete(borrowed);
        }
    }
}

impl Reactor for NativeReactor {
    /// Records the action to take when `fd` next becomes ready and arms
    /// the poller for `interest`. The waker is stored before arming: an
    /// already-ready fd can fire immediately.
    fn register(&self, fd: i32, interest: Interest, waker: Waker) {
        let event = match interest {
            Interest::Readable => Event::readable(fd as usize),
            Interest::Writable => Event::writable(fd as usize),
        };
        self.wakers.lock().unwrap().insert(fd, waker);
        self.arm(fd, event);
    }

    fn deregister(&self, fd: i32) {
        self.wakers.lock().unwrap().remove(&fd);
        self.disarm(fd);
    }

    /// Waits for readiness up to `timeout` and returns a waker for each
    /// fired fd. Oneshot disarms the poller entry on fire, but the waker
    /// stays registered until an explicit [`deregister`](Reactor::deregister)
    /// (an `io_block` waiter, on resume) or `release_fd` (a watcher). A
    /// `Fd.watch` owner re-arms by watching again.
    fn poll(&self, timeout: Option<Duration>) -> Vec<Waker> {
        let mut events = Events::new();
        match self.poller.wait(&mut events, timeout) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Vec::new(),
            Err(_) => return Vec::new(),
        }

        let wakers = self.wakers.lock().unwrap();
        events
            .iter()
            .filter_map(|event| with_readiness(*wakers.get(&(event.key as i32))?, &event))
            .collect()
    }
}

/// Fills a `Deliver` waker's readiness in from the fired event, and
/// passes a `Resume` waker through unchanged.
fn with_readiness(waker: Waker, event: &Event) -> Option<Waker> {
    match waker {
        Waker::Deliver { fd, pid, .. } => Some(Waker::Deliver {
            fd,
            pid,
            readiness: readiness_of(event),
        }),
        resume => Some(resume),
    }
}

/// The direction an event fired in, preferring readable then writable.
/// Anything else (hangup, poll error) is `Error`.
fn readiness_of(event: &Event) -> Readiness {
    if event.readable {
        Readiness::Readable
    } else if event.writable {
        Readiness::Writable
    } else {
        Readiness::Error
    }
}

/// The `IOReady` wire variant byte for a readiness direction.
fn io_variant(readiness: Readiness) -> u8 {
    match readiness {
        Readiness::Readable => IO_READY_READ,
        Readiness::Writable => IO_READY_WRITE,
        Readiness::Error => IO_READY_ERROR,
    }
}

/// Singleton reactor instance, created in [`init`].
static REACTOR: OnceLock<NativeReactor> = OnceLock::new();

/// Initializes the global reactor. Called once from the driver before
/// spawning the reactor thread.
pub fn init() {
    REACTOR.get_or_init(|| NativeReactor {
        poller: Poller::new().expect("failed to create I/O poller"),
        registered: Mutex::new(HashSet::new()),
        wakers: Mutex::new(HashMap::new()),
    });
}

/// Promotes a process from `WaitingIO` to `Runnable` if (and only if)
/// it is still parked. The state guard is essential: a process whose
/// state is `Running` (mid-`io_block`, before its context switch) or
/// already `Runnable` must not be transitioned, or `ProcessTable::
/// transition` trips its legal-edge assertion. Shared by the reactor
/// readiness path and `release_fd` so the two can't drift.
fn promote_io_waiter(sched: &mut NativeTable, pid: i64) {
    if sched
        .get(pid)
        .is_some_and(|process| process.state == ProcessState::WaitingIO)
    {
        sched.transition(pid, ProcessState::Runnable);
    }
}

/// Applies the wakers from one [`poll`](Reactor::poll) pass. `Resume`
/// wakers are promoted in a single `SCHED` critical section. `Deliver`
/// wakers send their `IOReady` afterward (`send_io_event` takes `SCHED`
/// itself), so payload glue never runs under the promote lock.
fn apply_wakers(wakers: Vec<Waker>) {
    {
        let mut sched = SCHED.lock().unwrap();
        for waker in &wakers {
            if let Waker::Resume(pid) = waker {
                promote_io_waiter(&mut sched, *pid);
            }
        }
        // Route the promoted waiters (staged in `pending_ready`) to the
        // work-stealing injectors. The `notify_all` below wakes the workers.
        publish_ready(&mut sched);
    }
    for waker in wakers {
        if let Waker::Deliver { fd, pid, readiness } = waker {
            send_io_event(pid, io_variant(readiness), fd as i64);
        }
    }
}

/// Wakes the reactor thread from its `poll` wait. Used during shutdown so
/// the reactor can observe [`SHUTDOWN`] and exit.
pub fn notify() {
    if let Some(reactor) = REACTOR.get() {
        let _ = reactor.poller.notify();
    }
}

/// Dedicated reactor thread loop. Drives the reactor's [`poll`](Reactor::poll)
/// and applies the returned wakers, waking workers afterward. Exits when
/// the global [`SHUTDOWN`] flag is set.
pub fn reactor_loop() {
    let reactor = REACTOR.get().expect("reactor not initialized");
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        let wakers = reactor.poll(Some(Duration::from_millis(50)));
        if wakers.is_empty() {
            continue;
        }
        apply_wakers(wakers);
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
    let Some(reactor) = REACTOR.get() else {
        return;
    };
    let pid = CURRENT_PID.with(|c| c.get());
    let (interest, readiness) = if interest == 1 {
        (Interest::Writable, Readiness::Writable)
    } else {
        (Interest::Readable, Readiness::Readable)
    };
    reactor.register(fd, interest, Waker::Deliver { fd, pid, readiness });
}

/// Removes a file descriptor from I/O readiness monitoring.
#[unsafe(no_mangle)]
pub extern "C" fn koja_rt_unwatch_fd(fd: i32) {
    if let Some(reactor) = REACTOR.get() {
        reactor.deregister(fd);
    }
}

/// Drops `fd` from the reactor's bookkeeping and wakes whoever was parked
/// on it, so closing an fd from one worker can't strand a process blocked
/// on it from another. Idempotent.
///
/// A process `io_block`-ed on `fd` is promoted `WaitingIO -> Runnable` (it
/// resumes, retries the syscall, and gets `EBADF`). A `Fd.watch` owner is
/// sent a synthetic `IOReady.Error` so its handler observes the hangup.
/// Without this, the poller entry is torn down and no further readiness
/// event ever fires for that fd.
pub(crate) fn release_fd(fd: i32) {
    let Some(reactor) = REACTOR.get() else {
        return;
    };
    let waker = reactor.wakers.lock().unwrap().remove(&fd);
    reactor.disarm(fd);

    match waker {
        Some(Waker::Resume(pid)) => {
            let mut sched = SCHED.lock().unwrap();
            promote_io_waiter(&mut sched, pid);
            publish_ready(&mut sched);
        }
        Some(Waker::Deliver { fd, pid, .. }) => {
            send_io_event(pid, IO_READY_ERROR, fd as i64);
        }
        None => {}
    }

    WORK_AVAILABLE.notify_all();
}

/// Suspends the current process until `fd` is ready for the given
/// [`Interest`], or a system/lifecycle message arrives. Called from
/// runtime I/O paths on `EAGAIN`. Returns `true` when a queued system
/// message interrupted the wait (the caller must stop retrying the
/// syscall and return to the run loop), `false` when fd readiness woke it.
///
/// State must be set to `WaitingIO` **before** `register`: the reactor's
/// wake guard checks `state == WaitingIO`, so a state of `Running` at fire
/// time silently drops the event and the process parks forever. Reverse
/// order means at worst a spurious resume.
///
/// `#[inline(never)]` is load-bearing. Callers retry this in a loop, and
/// the process can resume on a different worker thread each time, so the
/// `CURRENT_PID` read must not reuse a TLS base hoisted from before an
/// earlier iteration's switch. See the TLS caching note in
/// [`crate::scheduler`].
#[inline(never)]
pub fn io_block(fd: i32, interest: Interest) -> bool {
    let pid = CURRENT_PID.with(|c| c.get());

    // A queued system message must not be stranded behind the wait.
    // Bail so the caller can interrupt.
    let parked = {
        let mut sched = SCHED.lock().unwrap();
        if sched.has_system_mail(pid) {
            return true;
        }
        // A refused park means a kill landed mid-run: skip the registration
        // (no waiter to wake) and let the switch-out below be permanent.
        sched.try_park_io(pid)
    };
    if parked && let Some(reactor) = REACTOR.get() {
        reactor.register(fd, interest, Waker::Resume(pid));
    }

    yield_to_scheduler();

    if let Some(reactor) = REACTOR.get() {
        reactor.deregister(fd);
    }

    // A queued system message means a signal interrupted the wait, not readiness.
    SCHED.lock().unwrap().has_system_mail(pid)
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
        if get_errno() != EAGAIN {
            return Err(io::Error::last_os_error());
        }
        // A pending signal interrupts the wait so the process can handle it.
        if io_block(fd, interest) {
            return Err(io::Error::from_raw_os_error(EINTR));
        }
    }
}
