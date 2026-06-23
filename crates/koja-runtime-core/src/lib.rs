//! Target-agnostic core of the Koja runtime: the mailbox / wire message
//! layer, the shared allocator funnel, and the runtime **protocol**
//! traits that platform adapters implement.
//!
//! This crate names no platform — no `polling`, no inline assembly, no
//! OS threads (`libc` appears only for the allocator passthrough). The
//! native adapter (`koja-runtime-posix`) and the future cooperative adapters
//! (eval, WASI) depend on it and supply the capabilities behind the
//! protocol traits. The scheduling data structures (`ProcessTable`,
//! `ProcessState`, `scheduler_trace`) live here too, generic over the
//! executor's per-process execution state `X` and message representation `M`,
//! so the policy is shared while a process splits into an agnostic control
//! block plus an executor-owned execution state.
//!
//! See `koja/design/SCHEDULER-PROTOCOL.md` for the full design.

pub mod driver;
pub mod mailbox;
pub mod memory;
pub mod process_table;
pub mod protocol;
pub mod scheduler_trace;
pub mod wire;

pub use driver::CooperativeDriver;
pub use mailbox::{Mailbox, WaitTarget};
pub use process_table::{
    ProcessControlBlock, ProcessState, ProcessTable, Reclaim, ScheduleCounters, TimerEntry,
    slot_index,
};
pub use protocol::{
    Clock, Driver, Executor, Interest, Lifecycle, Message, MessageSource, Pid, Reactor, Readiness,
    SignalSource, Tag, Waker,
};
pub use scheduler_trace::{TraceEntry, TraceEvent};
pub use wire::{Envelope, OwnedPayload};
