//! The single-threaded cooperative [`Driver`]: the second
//! implementation of the run loop, shared by every cooperative backend
//! (eval today, WASI next).
//!
//! It mirrors the native `worker_loop` decision-for-decision (drain
//! signals into the entry process, promote due deadlines, fire due
//! timers, claim a runnable process and resume it, reclaim it on
//! switch-out, and otherwise idle until the nearest wakeup) minus the
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
use crate::protocol::{
    Clock, Driver, Executor, Lifecycle, MessageSource, Reactor, SignalSource, Waker,
};
use crate::timer_service::TimerService;
use crate::timer_wheel::Due;

/// Idle park cap when a process is `Running`/`WaitingIO`. Work is
/// imminent, so wake often. Mirrors the native worker loop.
const IDLE_CAP_ACTIVE: Duration = Duration::from_millis(10);
/// Idle park cap when nothing is active. Bounds signal-delivery latency
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
    /// Drain grace window armed when a `SIGTERM` arrives. Supplied by the
    /// adapter (which reads `KOJA_GRACE_MS`) so the core stays env-free.
    grace: Duration,
    reactor: R,
    signals: S,
    /// Timers, deadlines, and the drain-grace backstop. Shared with the
    /// adapter's park sites (which arm deadlines from process context),
    /// hence the `Rc` like [`Self::core`].
    timers: Rc<RefCell<TimerService<E::Message>>>,
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
        timers: Rc<RefCell<TimerService<E::Message>>>,
        executor: E,
        reactor: R,
        clock: C,
        signals: S,
        grace: Duration,
    ) -> Self {
        Self {
            clock,
            core,
            executor,
            grace,
            reactor,
            signals,
            timers,
        }
    }

    /// Drains latched OS signals into the entry process's system queue,
    /// one lifecycle message per fired signal. This is the cooperative
    /// mirror of the native loop's `poll_signals`.
    fn deliver_signals(&self) {
        let fired = self.signals.drain();
        if fired.is_empty() {
            return;
        }
        // A `SIGTERM` (`Shutdown`) starts the drain: refuse new spawns and
        // arm the grace deadline. The signal is still delivered to main so a
        // `Lifecycle`-aware program can shut itself down before the deadline.
        if fired.contains(&Lifecycle::Shutdown) && self.core.borrow_mut().enter_draining() {
            let now = self.clock.now();
            self.timers.borrow_mut().arm_grace(now + self.grace);
        }
        let main = self.core.borrow().main_pid();
        for event in fired {
            let message = self.executor.lifecycle_message(event);
            let leftover = self.core.borrow_mut().deliver(main, message);
            drop(leftover);
        }
    }

    /// Once the drain grace deadline passes, force-kills every straggler so
    /// the shutdown condition (`alive == 0`) fires. Drops the detached
    /// resources off the table borrow.
    fn enforce_grace(&self, now: Instant) {
        if !self.timers.borrow().grace_expired(now) {
            return;
        }
        let reclaimed = {
            let mut table = self.core.borrow_mut();
            if table.is_draining() {
                table.kill_all()
            } else {
                Vec::new()
            }
        };
        drop(reclaimed);
    }

    /// Fires everything due at `now`, promoting expired deadline waiters
    /// (re-validated by the table) and delivering due timers. Any
    /// undeliverable payload (target gone) is dropped off the table
    /// borrow.
    fn fire_due_timers(&self, now: Instant) {
        let due = self.timers.borrow_mut().advance(now);
        for entry in due {
            match entry {
                Due::Deliver {
                    envelope,
                    target_pid,
                } => {
                    let leftover = self.core.borrow_mut().deliver(target_pid, envelope);
                    drop(leftover);
                }
                Due::Promote { pid, fire_at } => {
                    self.core.borrow_mut().promote_expired(pid, fire_at);
                }
            }
        }
    }

    /// Parks the loop until the nearest deadline / timer, or up to a cap
    /// that bounds signal latency, polling the reactor for fd readiness
    /// meanwhile. Promotes any process the reactor resumes.
    fn idle(&self, now: Instant) {
        let nearest = self.timers.borrow().next_fire();
        let any_active = self.core.borrow().any_active();
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
                // Guard on `WaitingIO` like the native `promote_io_waiter`,
                // because a concurrent wake / kill may have already moved it,
                // and re-transitioning would trip the legal-edge assertion.
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

    /// Whether `pid` is currently parked in `WaitingIO`, the only state a
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
            self.fire_due_timers(now);
            self.enforce_grace(now);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Interest, Message, Pid, Readiness, Tag};
    use std::cell::Cell;

    struct MockMessage;
    impl Message for MockMessage {
        fn tag(&self) -> Tag {
            Tag::Lifecycle
        }
    }

    type MockTable = ProcessTable<(), MockMessage>;

    /// Executor that optionally marks a resumed process `Dead`, modelling a
    /// `main` that reacts to `Shutdown` by returning. Counts resumes so a
    /// test can tell whether `main` ever ran.
    struct MockExecutor {
        core: Rc<RefCell<MockTable>>,
        exit_on_resume: bool,
        resumes: Rc<Cell<usize>>,
    }

    impl Executor for MockExecutor {
        type Execution = ();
        type Continuation = ();
        type Message = MockMessage;

        fn resume(&self, pid: Pid, _continuation: ()) {
            self.resumes.set(self.resumes.get() + 1);
            if self.exit_on_resume {
                self.core.borrow_mut().mark_dead_if_alive(pid);
            }
        }
    }

    impl MessageSource<MockMessage> for MockExecutor {
        fn lifecycle_message(&self, _event: Lifecycle) -> MockMessage {
            MockMessage
        }
        fn io_ready_message(&self, _readiness: Readiness, _fd: i32) -> MockMessage {
            MockMessage
        }
    }

    struct MockReactor;
    impl Reactor for MockReactor {
        fn register(&self, _fd: i32, _interest: Interest, _waker: Waker) {}
        fn deregister(&self, _fd: i32) {}
        fn poll(&self, _timeout: Option<Duration>) -> Vec<Waker> {
            Vec::new()
        }
    }

    /// Clock whose logical time advances by `step` on every read, so the
    /// loop crosses a grace deadline deterministically without sleeping.
    struct MockClock {
        now: Rc<Cell<Instant>>,
        step: Duration,
    }
    impl Clock for MockClock {
        fn now(&self) -> Instant {
            let current = self.now.get();
            self.now.set(current + self.step);
            current
        }
    }

    /// Fires a single `SIGTERM` (`Shutdown`) on its first drain, nothing
    /// after: the injected signal the drain reacts to.
    struct OneShotSigterm {
        pending: Cell<bool>,
    }
    impl SignalSource for OneShotSigterm {
        fn install(&self) {}
        fn drain(&self) -> Vec<Lifecycle> {
            if self.pending.replace(false) {
                vec![Lifecycle::Shutdown]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn sigterm_lets_a_responsive_main_exit_before_the_deadline() {
        let core = Rc::new(RefCell::new(MockTable::new()));
        let main = core.borrow_mut().spawn((), None);
        assert_eq!(main, core.borrow().main_pid());

        let resumes = Rc::new(Cell::new(0));
        let start = Instant::now();
        let clock_now = Rc::new(Cell::new(start));
        let grace = Duration::from_secs(3600);
        CooperativeDriver::new(
            Rc::clone(&core),
            Rc::new(RefCell::new(TimerService::new())),
            MockExecutor {
                core: Rc::clone(&core),
                exit_on_resume: true,
                resumes: Rc::clone(&resumes),
            },
            MockReactor,
            MockClock {
                now: Rc::clone(&clock_now),
                step: Duration::from_millis(100),
            },
            OneShotSigterm {
                pending: Cell::new(true),
            },
            grace,
        )
        .run();

        assert!(resumes.get() >= 1, "main must have run to exit on its own");
        assert!(core.borrow().is_draining(), "SIGTERM should enter draining");
        assert!(
            clock_now.get() < start + grace,
            "responsive main should exit well before the grace deadline",
        );
    }

    #[test]
    fn sigterm_hard_kills_an_unresponsive_main_at_the_deadline() {
        let core = Rc::new(RefCell::new(MockTable::new()));
        let main = core.borrow_mut().spawn((), None);

        let resumes = Rc::new(Cell::new(0));
        let start = Instant::now();
        let clock_now = Rc::new(Cell::new(start));
        let grace = Duration::from_millis(250);
        CooperativeDriver::new(
            Rc::clone(&core),
            Rc::new(RefCell::new(TimerService::new())),
            // Never exits on its own: the only way `run` returns is the
            // grace-deadline hard-kill.
            MockExecutor {
                core: Rc::clone(&core),
                exit_on_resume: false,
                resumes: Rc::clone(&resumes),
            },
            MockReactor,
            MockClock {
                now: Rc::clone(&clock_now),
                step: Duration::from_millis(100),
            },
            OneShotSigterm {
                pending: Cell::new(true),
            },
            grace,
        )
        .run();

        assert!(
            clock_now.get() >= start + grace,
            "forced shutdown must wait for the grace deadline to pass",
        );
        assert!(core.borrow().get(main).is_none(), "straggler was killed");
        assert!(core.borrow().should_shutdown());
    }
}
