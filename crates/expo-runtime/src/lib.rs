//! Expo process runtime: cooperative coroutine scheduler with typed
//! mailboxes. Each process runs on its own stack and yields on
//! `receive` when its mailbox is empty.
//!
//! The [`alpha`] module is temporary scaffolding for the alpha LLVM
//! backend's auto-print `main` wrapper; see its module docs for the
//! deletion plan once `IO.puts` lands.

mod alpha;
mod ffi;
mod format;
mod fs;
mod panic;
mod reactor;
mod scheduler;
mod socket;
mod string;
mod system;
mod util;
