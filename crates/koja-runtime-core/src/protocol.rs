//! The runtime protocol: the capability traits a platform adapter
//! implements, plus the shared vocabulary types they trade in.
//!
//! The agnostic core owns scheduling *decisions*; an adapter supplies
//! *capabilities* — how a process runs and suspends ([`Executor`]), how
//! fd readiness arrives ([`Reactor`]), how the run loop is driven and
//! synchronized ([`Driver`]), and how time and OS signals are observed
//! ([`Clock`], [`SignalSource`]). The native adapter (`koja-runtime`)
//! is the first implementation; a single-threaded cooperative adapter
//! (eval, then WASI) is the second.
//!
//! Signatures here are the design surface and may refine as the native
//! implementations land against them.

use std::time::{Duration, Instant};

/// A scheduler-assigned process handle. Opaque to user code; the native
/// adapter packs a slot index and generation into it.
pub type Pid = i64;

/// Routing class of a mailbox message — which part of the receiver's
/// mailbox an incoming message lands in. A routing class, not a payload
/// shape. See [`crate::mailbox`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tag {
    /// Casts, call requests, timer fires: the business queue.
    Business,
    /// I/O readiness events from the reactor: the business queue.
    IOReady,
    /// Lifecycle signals: the system queue, drained before business.
    Lifecycle,
    /// A reply to an in-flight `Ref.call`: the one-shot reply slot.
    Reply,
}

/// A value that can ride a mailbox. The mailbox routes purely by
/// [`tag`](Message::tag); the concrete representation (byte `Envelope`
/// natively, a typed value cooperatively) is the executor's choice.
pub trait Message {
    fn tag(&self) -> Tag;
}

/// A lifecycle event delivered to the entry process. Discriminants are
/// the wire variant indices (`SIGTERM` -> `Shutdown`, `SIGINT` ->
/// `Interrupt`, `SIGHUP` -> `Reload`); see `koja/design/ABI.md`.
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

/// Why a process handed control back to the driver at a suspension
/// point. The parked state is already recorded in the process's control
/// block; this only tells the driver what to do next.
pub enum YieldReason {
    /// Parked on `receive` / reply.
    Blocked,
    /// Returned; the driver marks it dead and reclaims.
    Finished,
    /// Parked on fd readiness.
    WaitingIo,
}

/// What the reactor does when a registered fd becomes ready: resume a
/// process blocked on the fd (`io_block` path), or enqueue an `IOReady`
/// message for a watcher (`Fd.watch` path). Replaces the integer-offset
/// keyspace multiplexing called out in `koja/design/RUNTIME-GAPS.md`.
pub enum Waker {
    /// Deliver an `IOReady` message to `pid` for `fd`.
    Deliver { fd: i32, pid: Pid },
    /// Promote `pid` from `WaitingIo` to `Runnable`.
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
/// thread (`polling` crate); a cooperative driver calls it inline when
/// the ready queue empties (WASI `poll_oneoff`).
pub trait Reactor {
    fn register(&self, fd: i32, interest: Interest, waker: Waker);
    fn deregister(&self, fd: i32);
    /// Drive one readiness pass and return the wakers whose fds fired.
    fn poll(&self, timeout: Option<Duration>) -> Vec<Waker>;
}

/// Process activation and suspension — the abstraction that decouples
/// stackful-native from single-threaded-cooperative execution.
///
/// Native [`resume`](Executor::resume) context-switches into the
/// process stack and reads the post-switch state from the control
/// block; a cooperative executor re-enters the interpreter and returns
/// the [`YieldReason`] directly. The **release-before-suspend
/// invariant** holds for both: a suspension point releases its access to
/// the core before yielding and re-acquires it on resume.
pub trait Executor {
    /// Per-process execution state the core stores opaquely.
    type Context;
    /// Message representation carried in this executor's mailbox.
    type Message: Message;

    /// Run or resume `ctx` until it yields or finishes. Called by the
    /// [`Driver`] with the core lock / borrow released.
    fn resume(&self, pid: Pid, ctx: &mut Self::Context) -> YieldReason;
}

/// Owns the run loop and all synchronization. Native spins
/// `worker_count()` worker threads plus a reactor thread over a
/// `Mutex`-guarded core; a cooperative driver runs a single
/// ready-queue loop over the core with no lock. Replaces
/// `koja_rt_main_done`.
pub trait Driver {
    type Executor: Executor;

    /// Boot the runtime and run until the entry process dies.
    fn run(self);
}
