//! Koja process runtime: cooperative coroutine scheduler with typed
//! mailboxes. Each process runs on its own stack and yields on
//! `receive` when its mailbox is empty.
//!
//! The [`intrinsics`] module holds C-ABI runtime helpers called
//! from LLVM-emitted IR (`Global.print`, `Kernel.panic`, the
//! `Bits` pack/concat helpers).

mod ffi;
mod format;
mod fs;
mod intrinsics;
mod mailbox;
mod memory;
mod panic;
pub mod parse_text;
mod process_table;
mod reactor;
mod sched_trace;
mod scheduler;
mod socket;
mod string;
mod system;
mod tsan;
mod util;
mod wire;
