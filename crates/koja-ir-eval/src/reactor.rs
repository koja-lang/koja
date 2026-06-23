//! Eval's cooperative I/O reactor — the second [`Reactor`] implementor
//! after the native `koja-runtime-posix` adapter, and the shape a future
//! `koja-runtime-wasi` adapter reuses (`poll_oneoff` in place of POSIX
//! `poll(2)`).
//!
//! Native runs its reactor on a dedicated thread and context-switches
//! blocked processes; eval is single-threaded, so the reactor's
//! registration state is a thread-local [`REGISTRY`] shared between the
//! extern handlers that register fds ([`crate::externs::fd`]'s `io_block`
//! / `watch`) and the [`CooperativeDriver`](koja_runtime_core::CooperativeDriver)
//! that polls it when the ready queue empties. Both run on the one
//! interpreter thread, so a plain `RefCell` borrow held across single
//! operations is sufficient — there is nothing to lock against.
//!
//! Two readiness paths, one waker vocabulary (mirroring native):
//!
//! - **`io_block`** (a syscall hit `EAGAIN`, or a `Fd.block`): the process
//!   parks `WaitingIO` against a [`Waker::Resume`]; the driver promotes it
//!   when the fd fires. In *function* mode (a plain `koja eval` of a
//!   non-process body, driven by [`block_on`](crate::scheduler::block_on)
//!   with no driver loop) there is no one to promote it, so `io_block`
//!   blocks the single thread on the fd instead — both legal under the
//!   protocol (`koja/design/SCHEDULER-PROTOCOL.md`).
//! - **`watch`** (`Fd.watch`): registers a [`Waker::Deliver`]; the driver
//!   mints an `IOReady` message for the watcher when the fd fires.
//!
//! Registration is oneshot: a fired fd is dropped from the registry (a
//! `Fd.watch` owner re-arms by watching again), matching the native
//! poller's `PollMode::Oneshot`.
//!
//! ## Why pre-wait then delegate
//!
//! The native `koja_fd_read` / `koja_socket_accept` / … symbols couple the
//! syscall and its koja-heap marshaling with the *native* `io_block`
//! (native `SCHED` + asm context switch), which eval cannot drive. Eval's
//! cooperative wrappers ([`crate::externs::fd`], [`crate::externs::net`])
//! therefore [`io_block`] for readiness *first*, then call the native
//! symbol on an fd that is now ready — so its internal `block_until_ready`
//! completes on the first syscall and the native `io_block` is never
//! reached. The invariant that makes this sound: eval is single-threaded,
//! so nothing drains the fd between the readiness check and the immediate
//! delegated syscall.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::thread;
use std::time::Duration;

use koja_runtime_core::{Interest, Pid, Reactor, Readiness, Waker};

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
    /// idle [`poll`](Reactor::poll); single-threaded, so a `RefCell` is
    /// enough. The cooperative analog of native's global poller.
    static REGISTRY: RefCell<HashMap<i32, Registration>> = RefCell::new(HashMap::new());
}

/// Eval's [`Reactor`]: a thread-local fd registry polled with `poll(2)`.
/// Unit-sized — all state lives in [`REGISTRY`] so the extern handlers can
/// reach it without a handle.
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
/// reactor fills the fired direction in at `poll` time; the `readiness`
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

/// Suspend until `fd` is ready for `interest`, then return. The
/// cooperative core of every eval I/O wait: an already-ready fd returns
/// immediately (the common sequential case); otherwise a driven process
/// parks `WaitingIO` and yields to the driver, while a driver-less
/// function-mode run blocks the single thread on the fd.
pub(crate) async fn io_block(fd: i32, interest: Interest) {
    if ready_now(fd, interest) {
        return;
    }
    if !scheduler::runtime_installed() {
        blocking_poll(fd, interest);
        return;
    }
    let pid = scheduler::current_pid();
    // A refused park means a kill landed mid-run: skip the registration
    // (no waiter to wake) and let the yield below be permanent.
    if scheduler::park_io(pid) {
        arm(fd, interest, Waker::Resume(pid));
        YieldOnce::new().await;
        unwatch(fd);
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
/// now (or has errored / hung up — either way the delegated syscall should
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
/// `EINTR`; a genuine poll error returns so the delegated syscall surfaces
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
/// `Readable` (so the read sees EOF); a poll error / hangup elsewhere is
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

/// Fill a `Deliver` waker's readiness from the fired event; pass a
/// `Resume` waker through unchanged. Mirrors native's `with_readiness`.
fn fill(waker: Waker, readiness: Readiness) -> Waker {
    match waker {
        Waker::Deliver { fd, pid, .. } => Waker::Deliver { fd, pid, readiness },
        resume => resume,
    }
}

/// `poll(2)` timeout in milliseconds: a missing timeout blocks
/// indefinitely (`-1`); a present one clamps to `i32::MAX` ms.
fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        Some(duration) => duration.as_millis().min(i32::MAX as u128) as i32,
        None => -1,
    }
}
