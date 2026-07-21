//! Target-agnostic core of the Koja runtime: the mailbox / wire message
//! layer, the shared allocator funnel, and the runtime **protocol**
//! traits that platform adapters implement.
//!
//! This crate names no platform: no `polling`, no inline assembly, no
//! OS threads (`libc` appears only for the allocator passthrough). The
//! native adapter (`koja-runtime-posix`) and the future cooperative adapters
//! (eval, WASI) depend on it and supply the capabilities behind the
//! protocol traits. The scheduling data structures (`ProcessTable`,
//! `ProcessState`, `scheduler_trace`) live here too, generic over the
//! executor's per-process execution state `X` and message representation `M`,
//! so the policy is shared while a process splits into an agnostic control
//! block plus an executor-owned execution state.

pub mod driver;
mod lifecycle;
pub mod mailbox;
pub mod memory;
pub mod process_table;
pub mod protocol;
pub mod ready_queue;
pub mod scheduler_trace;
pub mod timer_service;
pub mod timer_wheel;
pub mod timing;
pub mod wire;

pub use driver::{CooperativeDriver, CooperativeRuntime};
pub use mailbox::{Mailbox, WaitTarget};
pub use process_table::{
    CrashInfo, Delivery, ExitNotice, ExitReason, IoPark, MailPark, Priority, ProcessState,
    ProcessTable, Reclaim, ReplyDelivery, ScheduleCounters, SwitchOutcome, Wake, slot_index,
};
pub use protocol::{
    Clock, Driver, Executor, Interest, Lifecycle, Message, MessageSource, Pid, Reactor, Readiness,
    SignalSource, Tag, Waker,
};
pub use ready_queue::ReadyQueue;
pub use scheduler_trace::{TraceEntry, TraceEvent};
pub use timer_service::TimerService;
pub use timer_wheel::Due;
pub use timing::duration_from_user_millis;
pub use wire::{Envelope, OwnedPayload};
