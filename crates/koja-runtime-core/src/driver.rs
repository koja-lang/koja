//! The single-threaded cooperative [`Driver`]: the second
//! implementation of the run loop, shared by every cooperative backend
//! (eval today, WASI next).
//!
//! It mirrors the native `worker_loop` decision-for-decision — drain
//! signals into the entry process, promote due deadlines, fire due
//! timers, claim a runnable process and resume it, reclaim it on
//! switch-out, and otherwise idle until the nearest wakeup — minus the
//! two things that make native multi-threaded: there are **no worker
//! threads** (one loop on the calling thread) and **no locking** (the
//! `ProcessTable` sits behind a bare `Rc<RefCell<…>>` borrow held only
//! across single operations, never across a `resume`).
//!
//! Because a cooperative executor keeps its suspended state inside the
//! executor (eval: a parked `Future`) rather than as a saved stack
//! pointer, the driver carries no resume token: it is bound to
//! `Executor<Continuation = ()>` and ignores `resume`'s return. Idle
//! waiting is delegated to [`Reactor::poll`] (which a real reactor backs
//! with `poll_oneoff`/`epoll`, and a fd-less backend with a plain
//! sleep), so the loop wakes for fd readiness, a deadline, or a fired
//! timer without busy-spinning.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::process_table::{ProcessState, ProcessTable};
use crate::protocol::{Clock, Driver, Executor, MessageSource, Reactor, SignalSource, Waker};

/// Idle park cap when a process is `Running`/`WaitingIO` — work is
/// imminent, so wake often. Mirrors the native worker loop.
const IDLE_CAP_ACTIVE: Duration = Duration::from_millis(10);
/// Idle park cap when nothing is active — bounds signal-delivery latency
/// without busy-spinning. Mirrors the native worker loop.
const IDLE_CAP_IDLE: Duration = Duration::from_millis(100);

/// A cooperative run loop over a shared, lock-free [`ProcessTable`].
///
/// Generic over the cooperative capabilities: the [`Executor`] `E` (which
/// also [`MessageSource`]s the lifecycle messages this loop delivers),
/// the [`Reactor`] `R`, the [`Clock`] `C`, and the [`SignalSource`] `S`.
pub struct CooperativeDriver<E, R, C, S>
where
    E: Executor<Continuation = ()> + MessageSource<E::Message>,
    R: Reactor,
    C: Clock,
    S: SignalSource,
{
    clock: C,
    core: Rc<RefCell<ProcessTable<E::Execution, E::Message>>>,
    executor: E,
    reactor: R,
    signals: S,
}

impl<E, R, C, S> CooperativeDriver<E, R, C, S>
where
    E: Executor<Continuation = ()> + MessageSource<E::Message>,
    R: Reactor,
    C: Clock,
    S: SignalSource,
{
    /// Assembles a driver over a shared core table and its cooperative
    /// capabilities. The caller boots the entry process (PID 1) into the
    /// table and the executor before calling [`Driver::run`].
    pub fn new(
        core: Rc<RefCell<ProcessTable<E::Execution, E::Message>>>,
        executor: E,
        reactor: R,
        clock: C,
        signals: S,
    ) -> Self {
        Self {
            clock,
            core,
            executor,
            reactor,
            signals,
        }
    }

    /// Drains latched OS signals into the entry process's system queue,
    /// one lifecycle message per fired signal — the cooperative mirror of
    /// the native loop's `poll_signals`.
    fn deliver_signals(&self) {
        let fired = self.signals.drain();
        if fired.is_empty() {
            return;
        }
        let main = self.core.borrow().main_pid();
        for event in fired {
            let message = self.executor.lifecycle_message(event);
            let leftover = self.core.borrow_mut().deliver(main, message);
            drop(leftover);
        }
    }

    /// Delivers every timer due at `now`, dropping any undeliverable
    /// payload (target gone) off the table borrow.
    fn fire_due_timers(&self, now: Instant) {
        let due = self.core.borrow_mut().take_due_timers(now);
        for entry in due {
            let leftover = self
                .core
                .borrow_mut()
                .deliver(entry.target_pid, entry.envelope);
            drop(leftover);
        }
    }

    /// Parks the loop until the nearest deadline / timer, or up to a cap
    /// that bounds signal latency, polling the reactor for fd readiness
    /// meanwhile. Promotes any process the reactor resumes.
    fn idle(&self, now: Instant) {
        let (nearest, any_active) = {
            let table = self.core.borrow();
            (table.nearest_wakeup(), table.any_active())
        };
        let cap = if any_active {
            IDLE_CAP_ACTIVE
        } else {
            IDLE_CAP_IDLE
        };
        let timeout = nearest
            .map(|wakeup| wakeup.saturating_duration_since(now))
            .unwrap_or(cap)
            .min(cap);
        for waker in self.reactor.poll(Some(timeout)) {
            match waker {
                // `io_block`: promote the waiter back onto the ready queue.
                // Guard on `WaitingIO` like the native `promote_io_waiter` —
                // a concurrent wake / kill may have already moved it, and
                // re-transitioning would trip the legal-edge assertion.
                Waker::Resume(pid) => {
                    if self.is_waiting_io(pid) {
                        self.core
                            .borrow_mut()
                            .transition(pid, ProcessState::Runnable);
                    }
                }
                // `Fd.watch`: mint the backend's `IOReady` message and
                // route it to the watcher, dropping any undeliverable
                // payload (watcher gone) off the table borrow.
                Waker::Deliver { fd, pid, readiness } => {
                    let message = self.executor.io_ready_message(readiness, fd);
                    let leftover = self.core.borrow_mut().deliver(pid, message);
                    drop(leftover);
                }
            }
        }
    }

    /// Whether `pid` is currently parked in `WaitingIO` — the only state a
    /// reactor `Resume` may legally promote from.
    fn is_waiting_io(&self, pid: crate::protocol::Pid) -> bool {
        self.core
            .borrow()
            .get(pid)
            .is_some_and(|process| process.state == ProcessState::WaitingIO)
    }
}

impl<E, R, C, S> Driver for CooperativeDriver<E, R, C, S>
where
    E: Executor<Continuation = ()> + MessageSource<E::Message>,
    R: Reactor,
    C: Clock,
    S: SignalSource,
{
    type Executor = E;

    fn run(self) {
        self.signals.install();
        loop {
            self.deliver_signals();
            let now = self.clock.now();
            self.core.borrow_mut().promote_due_deadlines(now);
            self.fire_due_timers(now);

            // Bind before matching so the `borrow_mut` temporary is dropped
            // here, not held across the arms (and across `resume`, which
            // reaches back into the table).
            let claimed = self.core.borrow_mut().claim_next();
            match claimed {
                Some(pid) => {
                    // Resume with the borrow released (release-before-suspend):
                    // the process reaches back into the table to park / peek
                    // its mailbox while it runs.
                    let () = self.executor.resume(pid, ());
                    let reclaim = self.core.borrow_mut().after_switch(pid);
                    drop(reclaim);
                }
                None => {
                    if self.core.borrow().should_shutdown() {
                        break;
                    }
                    self.idle(now);
                }
            }

            if self.core.borrow().should_shutdown() {
                break;
            }
        }
    }
}
