//! The single-threaded cooperative [`Driver`]: the second
//! implementation of the run loop, shared by every cooperative backend
//! (eval today, WASI next).
//!
//! It mirrors the native `worker_loop` decision-for-decision (drain
//! signals into the entry process, promote due deadlines, fire due
//! timers, claim a runnable process and resume it, reclaim it on
//! switch-out, and otherwise idle until the nearest wakeup) minus the
//! two things that make native multi-threaded: there are **no worker
//! threads** (one loop on the calling thread) and **no work stealing**
//! (the driver owns one [`ReadyQueue`], fed by the wake facts the shared
//! [`ProcessTable`] returns, and the table's locks are uncontended).
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

use crate::process_table::{ProcessTable, SwitchOutcome};
use crate::protocol::{
    Clock, Driver, Executor, Lifecycle, Message, MessageSource, Pid, Reactor, SignalSource, Waker,
};
use crate::ready_queue::ReadyQueue;
use crate::timer_service::TimerService;
use crate::timer_wheel::Due;

/// Idle park cap when a process is `Running`/`WaitingIO`. Work is
/// imminent, so wake often. Mirrors the native worker loop.
const IDLE_CAP_ACTIVE: Duration = Duration::from_millis(10);
/// Idle park cap when nothing is active. Bounds signal-delivery latency
/// without busy-spinning. Mirrors the native worker loop.
const IDLE_CAP_IDLE: Duration = Duration::from_millis(100);

/// The shared state a cooperative run loops over: the table plus the
/// driver-owned ready queue and timer service. Each handle is an `Rc`
/// because the adapter's wake and park sites (`spawn`, `send`, `reply`,
/// deadline arming) run in process context and reach the same state.
pub struct CooperativeRuntime<X, M> {
    pub core: Rc<ProcessTable<X, M>>,
    pub ready: Rc<RefCell<ReadyQueue>>,
    pub timers: Rc<RefCell<TimerService<M>>>,
}

impl<X, M: Message> CooperativeRuntime<X, M> {
    /// A fresh table with an empty ready queue and timer service.
    pub fn new() -> Self {
        Self {
            core: Rc::new(ProcessTable::new()),
            ready: Rc::new(RefCell::new(ReadyQueue::new())),
            timers: Rc::new(RefCell::new(TimerService::new())),
        }
    }
}

impl<X, M: Message> Default for CooperativeRuntime<X, M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<X, M> Clone for CooperativeRuntime<X, M> {
    fn clone(&self) -> Self {
        Self {
            core: Rc::clone(&self.core),
            ready: Rc::clone(&self.ready),
            timers: Rc::clone(&self.timers),
        }
    }
}

/// A cooperative run loop over the shared, internally synchronized
/// [`ProcessTable`].
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
    executor: E,
    /// Drain grace window armed when a `SIGTERM` arrives. Supplied by the
    /// adapter (which reads `KOJA_GRACE_MS`) so the core stays env-free.
    grace: Duration,
    reactor: R,
    runtime: CooperativeRuntime<E::Execution, E::Message>,
    signals: S,
}

impl<E, R, C, S> CooperativeDriver<E, R, C, S>
where
    E: Executor<Continuation = ()> + MessageSource<E::Message>,
    R: Reactor,
    C: Clock,
    S: SignalSource,
{
    /// Assembles a driver over the shared runtime state and its
    /// cooperative capabilities. The caller boots the entry process
    /// (PID 1) into the table, the executor, and the ready queue before
    /// calling [`Driver::run`].
    pub fn new(
        runtime: CooperativeRuntime<E::Execution, E::Message>,
        executor: E,
        reactor: R,
        clock: C,
        signals: S,
        grace: Duration,
    ) -> Self {
        Self {
            clock,
            executor,
            grace,
            reactor,
            runtime,
            signals,
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
        if fired.contains(&Lifecycle::Shutdown) && self.runtime.core.enter_draining() {
            let now = self.clock.now();
            self.runtime.timers.borrow_mut().arm_grace(now + self.grace);
        }
        let main = self.runtime.core.main_pid();
        for event in fired {
            let message = self.executor.lifecycle_message(event);
            self.route_delivery(main, message);
        }
    }

    /// Delivers `message` to `pid`, enqueues the wake it may produce, and
    /// drops any leftover (bounced or displaced message) off the table's
    /// locks.
    fn route_delivery(&self, pid: Pid, message: E::Message) {
        let delivery = self.runtime.core.deliver(pid, message);
        if let Some(wake) = delivery.wake {
            self.runtime.ready.borrow_mut().push(wake);
        }
        drop(delivery.leftover);
    }

    /// Once the drain grace deadline passes, force-kills every straggler so
    /// the shutdown condition (`alive == 0`) fires. Drops the detached
    /// resources off the table's locks.
    fn enforce_grace(&self, now: Instant) {
        if !self.runtime.timers.borrow().grace_expired(now) {
            return;
        }
        let reclaimed = if self.runtime.core.is_draining() {
            self.runtime.core.kill_all()
        } else {
            Vec::new()
        };
        drop(reclaimed);
    }

    /// Fires everything due at `now`, promoting expired deadline waiters
    /// (re-validated by the table) and delivering due timers. Any
    /// undeliverable payload (target gone) is dropped off the table's
    /// locks.
    fn fire_due_timers(&self, now: Instant) {
        let due = self.runtime.timers.borrow_mut().advance(now);
        for entry in due {
            match entry {
                Due::Deliver {
                    envelope,
                    target_pid,
                } => self.route_delivery(target_pid, envelope),
                Due::Promote { pid, fire_at } => {
                    if let Some(wake) = self.runtime.core.promote_expired(pid, fire_at) {
                        self.runtime.ready.borrow_mut().push(wake);
                    }
                }
            }
        }
    }

    /// Parks the loop until the nearest deadline / timer, or up to a cap
    /// that bounds signal latency, polling the reactor for fd readiness
    /// meanwhile. Promotes any process the reactor resumes.
    fn idle(&self, now: Instant) {
        let nearest = self.runtime.timers.borrow().next_fire();
        let cap = if self.runtime.core.any_active() {
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
                // The table re-validates it is still `WaitingIO`, because a
                // concurrent wake / kill may have already moved it.
                Waker::Resume(pid) => {
                    if let Some(wake) = self.runtime.core.promote_io_waiter(pid) {
                        self.runtime.ready.borrow_mut().push(wake);
                    }
                }
                // `Fd.watch`: mint the backend's `IOReady` message and
                // route it to the watcher.
                Waker::Deliver { fd, pid, readiness } => {
                    let message = self.executor.io_ready_message(readiness, fd);
                    self.route_delivery(pid, message);
                }
            }
        }
    }

    /// Pops ready candidates until one claims, skipping stale entries
    /// (killed or already resumed). `None` when the queue runs dry.
    fn claim_next(&self) -> Option<Pid> {
        loop {
            let pid = self.runtime.ready.borrow_mut().pop()?;
            if self.runtime.core.try_claim(pid) {
                return Some(pid);
            }
        }
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

            match self.claim_next() {
                Some(pid) => {
                    // Resume with no queue borrow held (release-before-suspend):
                    // the process reaches back into the table to park / peek
                    // its mailbox while it runs.
                    let () = self.executor.resume(pid, ());
                    match self.runtime.core.after_switch(pid) {
                        SwitchOutcome::Requeue(wake) => self.runtime.ready.borrow_mut().push(wake),
                        SwitchOutcome::Reclaimed(reclaim) => drop(reclaim),
                        SwitchOutcome::Parked => {}
                    }
                }
                None => {
                    if self.runtime.core.should_shutdown() {
                        break;
                    }
                    self.idle(now);
                }
            }

            if self.runtime.core.should_shutdown() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_table::{Priority, Wake};
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
        core: Rc<MockTable>,
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
                self.core.mark_dead_if_alive(pid);
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

    /// Spawns the entry process and enqueues it, as `run_program` does.
    fn boot_main(runtime: &CooperativeRuntime<(), MockMessage>) -> Pid {
        let main = runtime.core.spawn((), None).unwrap();
        runtime.ready.borrow_mut().push(Wake {
            pid: main,
            priority: Priority::Normal,
        });
        main
    }

    #[test]
    fn sigterm_lets_a_responsive_main_exit_before_the_deadline() {
        let runtime: CooperativeRuntime<(), MockMessage> = CooperativeRuntime::new();
        let main = boot_main(&runtime);
        assert_eq!(main, runtime.core.main_pid());

        let core = Rc::clone(&runtime.core);
        let resumes = Rc::new(Cell::new(0));
        let start = Instant::now();
        let clock_now = Rc::new(Cell::new(start));
        let grace = Duration::from_secs(3600);
        CooperativeDriver::new(
            runtime,
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
        assert!(core.is_draining(), "SIGTERM should enter draining");
        assert!(
            clock_now.get() < start + grace,
            "responsive main should exit well before the grace deadline",
        );
    }

    #[test]
    fn sigterm_hard_kills_an_unresponsive_main_at_the_deadline() {
        let runtime: CooperativeRuntime<(), MockMessage> = CooperativeRuntime::new();
        let main = boot_main(&runtime);

        let core = Rc::clone(&runtime.core);
        let resumes = Rc::new(Cell::new(0));
        let start = Instant::now();
        let clock_now = Rc::new(Cell::new(start));
        let grace = Duration::from_millis(250);
        CooperativeDriver::new(
            runtime,
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
        assert!(!core.is_alive(main), "straggler was killed");
        assert!(core.should_shutdown());
    }
}
