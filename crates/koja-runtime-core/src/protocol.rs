//! The runtime protocol: the capability traits a platform adapter
//! implements, plus the shared vocabulary types they trade in.
//!
//! The agnostic core owns scheduling *decisions*, while an adapter
//! supplies *capabilities*: how a process runs and suspends
//! ([`Executor`]), how fd readiness arrives ([`Reactor`]), how the run
//! loop is driven and synchronized ([`Driver`]), and how time and OS
//! signals are observed ([`Clock`], [`SignalSource`]). The native
//! adapter (`koja-runtime-posix`) is the first implementation, and a
//! single-threaded cooperative adapter (eval, then WASI) is the second.
//!
//! Signatures here are the design surface and may refine as the native
//! implementations land against them.

use std::time::{Duration, Instant};

/// A scheduler-assigned process handle. Opaque to user code. The native
/// adapter packs a slot index and generation into it.
pub type Pid = i64;

/// Routing class of a mailbox message: which part of the receiver's
/// mailbox an incoming message lands in. A routing class, not a payload
/// shape. See [`crate::mailbox`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tag {
    /// Casts, call requests, timer fires: the business queue.
    Business,
    /// A monitor's exit notification: the business queue.
    ExitSignal,
    /// I/O readiness events from the reactor: the business queue.
    IOReady,
    /// Lifecycle signals: the system queue, drained before business.
    Lifecycle,
    /// A reply to an in-flight `Ref.call`: the one-shot reply slot.
    Reply,
}

/// A value that can ride a mailbox. The mailbox routes purely by
/// [`tag`](Message::tag). The concrete representation (byte `Envelope`
/// natively, a typed value cooperatively) is the executor's choice.
pub trait Message {
    fn tag(&self) -> Tag;
}

/// Mints the protocol messages the
/// [`CooperativeDriver`](crate::CooperativeDriver) must synthesize on its
/// own: lifecycle signals drained from the [`SignalSource`] and I/O
/// readiness events the [`Reactor`] reports for a `Fd.watch` owner. The
/// native driver builds these inline in its own loop (signals into the
/// system queue, `IOReady`s via `send_io_event`), so only cooperative
/// backends (eval, then WASI) implement this. Carries the message as a
/// type parameter (rather than an associated type) so the driver can bind
/// it to the executor's `Message` (`E: MessageSource<E::Message>`)
/// without a second `Message` associated type to disambiguate.
pub trait MessageSource<M: Message> {
    /// Build the message delivered to the entry process for `event`.
    fn lifecycle_message(&self, event: Lifecycle) -> M;

    /// Build the `IOReady` message for a watched `fd` that became ready in
    /// `readiness`, delivered to its watcher's business queue.
    fn io_ready_message(&self, readiness: Readiness, fd: i32) -> M;
}

/// A lifecycle event delivered to the entry process. Discriminants are
/// the variant bytes stamped into lifecycle envelopes (`SIGTERM` ->
/// `Shutdown`, `SIGINT` -> `Interrupt`, `SIGHUP` -> `Reload`), so
/// emitted receive arms match on them and they must not be renumbered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Shutdown = 0,
    Interrupt = 1,
    Reload = 2,
}

/// Whether the reactor should wake for readable or writable readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interest {
    Readable,
    Writable,
}

/// Which direction a registered fd became ready in. Set by the reactor
/// from the fired event, then carried to the watcher as the `IOReady`
/// variant. `Error` covers hangup / poll error / a forced `release_fd`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Error,
    Readable,
    Writable,
}

/// What the reactor does when a registered fd becomes ready: resume a
/// process blocked on the fd (`io_block` path), or enqueue an `IOReady`
/// message for a watcher (`Fd.watch` path). A typed action, replacing
/// the old scheme that multiplexed pid keys and offset fd keys into one
/// integer keyspace.
///
/// Registered as the action to take. Returned by [`Reactor::poll`] with
/// the `Deliver` `readiness` filled in from the event that fired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Waker {
    /// Deliver an `IOReady` message to `pid` for `fd`.
    Deliver {
        fd: i32,
        pid: Pid,
        readiness: Readiness,
    },
    /// Promote `pid` from `WaitingIO` to `Runnable`.
    Resume(Pid),
}

/// The runtime's source of monotonic time, used for receive deadlines
/// and timer firing.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// OS signal capture: latch the signals of interest, then drain them
/// into [`Lifecycle`] events on the driver's schedule.
pub trait SignalSource {
    fn install(&self);
    fn drain(&self) -> Vec<Lifecycle>;
}

/// fd readiness. Native drives [`poll`](Reactor::poll) on a dedicated
/// thread (`polling` crate). A cooperative driver calls it inline when
/// the ready queue empties (WASI `poll_oneoff`).
pub trait Reactor {
    fn register(&self, fd: i32, interest: Interest, waker: Waker);
    fn deregister(&self, fd: i32);
    /// Drive one readiness pass and return the wakers whose fds fired.
    fn poll(&self, timeout: Option<Duration>) -> Vec<Waker>;
}

/// Process activation and suspension: the abstraction that decouples
/// stackful-native from single-threaded-cooperative execution.
///
/// [`resume`](Executor::resume) enters or continues a process until it
/// next yields control back. It deliberately trades only a small `Copy`
/// [`Continuation`](Executor::Continuation) token (native: the saved
/// stack pointer) rather than `&mut Execution`: the native switch
/// releases the core lock across the context switch, and the running
/// process reads its own execution state mid-switch, so a borrow can't
/// span the suspension point. The [`Driver`] reads the prior token out
/// of the table under the lock, drops the lock, calls `resume`, then
/// stores the returned token back. It consults the process's
/// control-block state (the authoritative record) to decide what to do
/// next, since a concurrent kill or wake may have landed meanwhile.
///
/// The **release-before-suspend invariant** holds for both backends: a
/// suspension point releases its access to the core before yielding and
/// re-acquires it on resume.
pub trait Executor {
    /// Per-process execution state, stored opaquely in each process's
    /// [`ProcessControlBlock`](crate::process_table::ProcessControlBlock).
    type Execution;
    /// The `Copy` resume token the driver marshals in and out of the
    /// table around a [`resume`](Executor::resume) (native: the saved
    /// stack pointer). A projection of [`Execution`](Executor::Execution)
    /// small enough to move across the lock boundary by value.
    type Continuation: Copy;
    /// Message representation carried in this executor's mailbox.
    type Message: Message;

    /// Enter or continue process `pid` from `continuation`, running it
    /// until it yields, and return the token to resume it next time.
    /// Called by the [`Driver`] with the core lock / borrow released.
    fn resume(&self, pid: Pid, continuation: Self::Continuation) -> Self::Continuation;
}

/// Owns the run loop and all synchronization. Native spins
/// `worker_count()` worker threads plus a reactor thread over a
/// `Mutex`-guarded core. A cooperative driver runs a single
/// ready-queue loop over the core with no lock. Replaces
/// `koja_rt_main_done`.
pub trait Driver {
    type Executor: Executor;

    /// Boot the runtime and run until the entry process dies.
    fn run(self);
}
