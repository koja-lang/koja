//! Expo process runtime: cooperative coroutine scheduler with typed
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
mod panic;
mod reactor;
mod scheduler;
mod socket;
mod string;
mod system;
mod util;
