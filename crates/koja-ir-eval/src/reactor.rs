//! Eval's cooperative I/O reactor, the second [`Reactor`] implementor
//! after the native `koja-runtime-posix` adapter. Eval is
//! single-threaded, so registration state is a thread-local
//! [`REGISTRY`] shared between the extern handlers that register fds
//! and the [`CooperativeDriver`](koja_runtime_core::CooperativeDriver)
//! that polls it when the ready queue empties.
//!
//! Two readiness paths. [`io_block`] parks the process `WaitingIO`
//! against a [`Waker::Resume`], or blocks the thread on the fd in
//! function mode where no driver runs. `watch` registers a
//! [`Waker::Deliver`] and the driver mints an `IOReady` message when
//! the fd fires. Registration is oneshot.
//!
//! The native `koja_fd_*` and `koja_socket_*` symbols park through the
//! native `io_block`, which eval cannot drive. Eval's wrappers
//! therefore wait for readiness here first and only then call the
//! native symbol, so its own wait completes on the first syscall.
//! This is sound because eval is single-threaded and nothing drains
//! the fd between the readiness check and the delegated syscall.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::thread;
use std::time::Duration;

use koja_runtime_core::{Interest, IoPark, Pid, Reactor, Readiness, Waker};

use crate::scheduler::{self, YieldOnce};

/// `poll(2)` event bits (identical on macOS and Linux).
const POLLIN: i16 = 0x1;
const POLLOUT: i16 = 0x4;
const POLLERR: i16 = 0x8;
const POLLHUP: i16 = 0x10;
const POLLNVAL: i16 = 0x20;

/// POSIX `struct pollfd` (identical layout on macOS and Linux).
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

/// One armed fd: the `poll(2)` event mask it waits on and the action to
/// take when it fires.
struct Registration {
    events: i16,
    waker: Waker,
}

thread_local! {
    /// The reactor's armed fds for the in-flight run. Mutated by the
    /// extern handlers (`io_block` / `watch`) and read by the driver's
    /// idle [`poll`](Reactor::poll). Single-threaded, so a `RefCell` is
    /// enough. The cooperative analog of native's global poller.
    static REGISTRY: RefCell<HashMap<i32, Registration>> = RefCell::new(HashMap::new());
}

/// Eval's [`Reactor`]: a thread-local fd registry polled with `poll(2)`.
/// Unit-sized, since all state lives in [`REGISTRY`] so the extern
/// handlers can reach it without a handle.
pub(crate) struct EvalReactor;

impl Reactor for EvalReactor {
    fn register(&self, fd: i32, interest: Interest, waker: Waker) {
        arm(fd, interest, waker);
    }

    fn deregister(&self, fd: i32) {
        REGISTRY.with(|registry| registry.borrow_mut().remove(&fd));
    }

    /// One readiness pass over the armed fds, up to `timeout`. Returns a
    /// waker per fired fd (with `Deliver` readiness filled from the event),
    /// dropping each fired fd from the registry (oneshot). With nothing
    /// armed, sleeps out the timeout so the driver idles instead of
    /// busy-spinning.
    fn poll(&self, timeout: Option<Duration>) -> Vec<Waker> {
        let armed: Vec<(i32, i16, Waker)> = REGISTRY.with(|registry| {
            registry
                .borrow()
                .iter()
                .map(|(fd, reg)| (*fd, reg.events, reg.waker))
                .collect()
        });
        if armed.is_empty() {
            if let Some(timeout) = timeout {
                thread::sleep(timeout);
            }
            return Vec::new();
        }

        let mut pollfds: Vec<PollFd> = armed
            .iter()
            .map(|(fd, events, _)| PollFd {
                fd: *fd,
                events: *events,
                revents: 0,
            })
            .collect();
        let ready = unsafe {
            poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as u32,
                timeout_ms(timeout),
            )
        };
        if ready <= 0 {
            return Vec::new();
        }

        let mut fired = Vec::new();
        for ((fd, events, waker), pollfd) in armed.iter().zip(pollfds.iter()) {
            if pollfd.revents == 0 {
                continue;
            }
            fired.push(fill(*waker, readiness_for(*events, pollfd.revents)));
            REGISTRY.with(|registry| registry.borrow_mut().remove(fd));
        }
        fired
    }
}

/// Register `fd` for one `IOReady` delivery to `pid` (`Fd.watch`). The
/// reactor fills the fired direction in at `poll` time. The `readiness`
/// here is the registered interest, a placeholder until then.
pub(crate) fn watch(fd: i32, interest: Interest, pid: Pid) {
    let readiness = match interest {
        Interest::Readable => Readiness::Readable,
        Interest::Writable => Readiness::Writable,
    };
    arm(fd, interest, Waker::Deliver { fd, pid, readiness });
}

/// Drop `fd` from readiness monitoring (`Fd.unwatch`). Idempotent.
pub(crate) fn unwatch(fd: i32) {
    REGISTRY.with(|registry| registry.borrow_mut().remove(&fd));
}

/// Suspend until `fd` is ready for `interest`, then return whether the
/// wait was *interrupted* (resumed without the fd becoming ready, i.e.
/// woken by a message rather than readiness). The cooperative core of
/// every eval I/O wait: an already-ready fd returns `false` immediately
/// (the common sequential case). Otherwise a driven process parks
/// `WaitingIO` and yields to the driver, while a driver-less function-mode
/// run blocks the single thread on the fd.
pub(crate) async fn io_block(fd: i32, interest: Interest) -> bool {
    if ready_now(fd, interest) {
        return false;
    }
    if !scheduler::runtime_installed() {
        blocking_poll(fd, interest);
        return false;
    }
    let pid = scheduler::current_pid();
    match scheduler::park_io(pid) {
        // A queued system message must not be stranded behind the wait:
        // report the interrupt so the caller handles the signal.
        IoPark::SystemMail => true,
        IoPark::Parked => {
            arm(fd, interest, Waker::Resume(pid));
            YieldOnce::new().await;
            unwatch(fd);
            // Resumed without readiness means a message woke us, so report
            // the interrupt.
            !ready_now(fd, interest)
        }
        // A refused park means a kill landed mid-run: skip the registration
        // (no waiter to wake). The process never resumes past the next
        // yield, so the answer is moot.
        IoPark::Refused => false,
    }
}

/// Insert (or replace) `fd`'s registration. The last `register` for an fd
/// wins, matching the native poller's one-entry-per-fd semantics.
fn arm(fd: i32, interest: Interest, waker: Waker) {
    REGISTRY.with(|registry| {
        registry.borrow_mut().insert(
            fd,
            Registration {
                events: events_for(interest),
                waker,
            },
        )
    });
}

/// A zero-timeout `poll(2)`: whether `fd` is ready for `interest` right
/// now, or has errored / hung up (either way the delegated syscall should
/// run rather than park on a dead fd).
fn ready_now(fd: i32, interest: Interest) -> bool {
    let events = events_for(interest);
    let mut pollfd = PollFd {
        fd,
        events,
        revents: 0,
    };
    let ready = unsafe { poll(&mut pollfd, 1, 0) };
    ready > 0 && pollfd.revents & (events | POLLERR | POLLHUP | POLLNVAL) != 0
}

/// Block the calling thread on `fd` until it is ready for `interest`
/// (function mode: no driver to resume a parked process). Retries across
/// `EINTR`. A genuine poll error returns so the delegated syscall surfaces
/// it (a broken fd fails with a real errno, never `EAGAIN`, so the native
/// `io_block` is still not reached).
fn blocking_poll(fd: i32, interest: Interest) {
    let events = events_for(interest);
    loop {
        let mut pollfd = PollFd {
            fd,
            events,
            revents: 0,
        };
        let ready = unsafe { poll(&mut pollfd, 1, -1) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if ready > 0 && pollfd.revents & (events | POLLERR | POLLHUP | POLLNVAL) != 0 {
            return;
        }
    }
}

/// The `poll(2)` event mask for an [`Interest`].
fn events_for(interest: Interest) -> i16 {
    match interest {
        Interest::Readable => POLLIN,
        Interest::Writable => POLLOUT,
    }
}

/// The direction a fired fd became ready in, from its registered interest
/// and the `revents` the kernel set. Hangup on a reader surfaces as
/// `Readable` (so the read sees EOF). A poll error / hangup elsewhere is
/// `Error`.
fn readiness_for(events: i16, revents: i16) -> Readiness {
    if events & POLLIN != 0 && revents & (POLLIN | POLLHUP) != 0 {
        Readiness::Readable
    } else if events & POLLOUT != 0 && revents & POLLOUT != 0 {
        Readiness::Writable
    } else {
        Readiness::Error
    }
}

/// Fill a `Deliver` waker's readiness from the fired event, passing a
/// `Resume` waker through unchanged. Mirrors native's `with_readiness`.
fn fill(waker: Waker, readiness: Readiness) -> Waker {
    match waker {
        Waker::Deliver { fd, pid, .. } => Waker::Deliver { fd, pid, readiness },
        resume => resume,
    }
}

/// `poll(2)` timeout in milliseconds: a missing timeout blocks
/// indefinitely (`-1`), and a present one clamps to `i32::MAX` ms.
fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        Some(duration) => duration.as_millis().min(i32::MAX as u128) as i32,
        None => -1,
    }
}
