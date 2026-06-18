//! Target-agnostic core of the Koja runtime: the mailbox / wire message
//! layer, the shared allocator funnel, and the runtime **protocol**
//! traits that platform adapters implement.
//!
//! This crate names no platform — no `polling`, no inline assembly, no
//! OS threads (`libc` appears only for the allocator passthrough). The
//! native adapter (`koja-runtime`) and the future cooperative adapters
//! (eval, WASI) depend on it and supply the capabilities behind the
//! protocol traits. The scheduling data structures (`ProcessTable`,
//! `ProcessState`, `sched_trace`) join this crate when `Process` is
//! split into an agnostic control block plus an executor-owned
//! execution context.
//!
//! See `koja/design/SCHEDULER-PROTOCOL.md` for the full design.

pub mod mailbox;
pub mod memory;
pub mod protocol;
pub mod wire;

pub use mailbox::{Mailbox, WaitTarget};
pub use protocol::{
    Clock, Driver, Executor, Interest, Lifecycle, Message, Pid, Reactor, SignalSource, Tag, Waker,
    YieldReason,
};
pub use wire::{Envelope, OwnedPayload};
