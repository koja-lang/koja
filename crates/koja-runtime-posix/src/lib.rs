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
mod memory;
mod panic;
pub mod parse_text;
mod reactor;
mod scheduler;
pub mod signals;
mod socket;
mod string;
mod system;
mod tsan;
mod util;

// The mailbox / wire message layer lives in the platform-agnostic
// core. This native adapter re-exports the modules so
// `crate::mailbox` / `crate::wire` paths keep resolving.
pub(crate) use koja_runtime_core::{mailbox, wire};
