//! Tree-walking interpreter for sealed [`koja_ir::IRProgram`]s
//! and [`koja_ir::IRScript`]s — peer to
//! [`koja-ir-llvm`](../koja_ir_llvm/index.html) but
//! interpreting in-process instead of emitting native code.
//!
//! [`Interpreter::run_program`] / [`Interpreter::run_script`] return
//! the [`Value`] produced by the entry / body, or a [`RuntimeError`]
//! for the recoverable failure modes.
//!
//! Process scope: program mode boots the `Process` entry as PID 1 and
//! runs it over the shared cooperative scheduler — eval is the second
//! implementation of the `koja-runtime-core` protocol after the native
//! `koja-runtime-posix` adapter (see [`crate::scheduler`]). `spawn`
//! creates real child processes; `receive` parks against the core mailbox
//! for business, `Lifecycle`, and `after`-timeout traffic; and the full
//! `Ref`/`ReplyTo` intrinsic surface (`cast`, `call` with reply-token
//! await, `signal`, `send_after`, `self_ref`, `alive?`, `kill`, and
//! `ReplyTo.send`) routes through the core (see [`crate::intrinsics`]).
//! I/O is cooperative too: eval's [`crate::reactor`] backs `io_block` /
//! `Fd.watch` (delivering `IOReady` messages) and the fd / socket read,
//! write, and accept paths park on readiness instead of blocking the
//! shared driver thread.
//!
//! Hard contract: zero dependency on the v1 `koja-ir` / `koja-ir-eval`
//! path. Sealed `IRProgram` / `IRScript` from `koja-ir` are the
//! only inputs.

// Keep `koja-runtime-posix`'s rlib in the link graph even if the direct
// Rust-path uses (e.g. [`crate::scheduler::EvalSignals`], which calls
// `koja_runtime::signals`) ever go away: the `#[unsafe(no_mangle)] pub
// extern "C" fn`s referenced by [`crate::externs`] resolve at link time,
// and without a `use` cargo would skip the rlib and the C symbols would
// come up undefined.
use koja_runtime as _;

// Pull `boring-sys` into the link graph so its `#[link(name =
// "crypto", ...)]` / `#[link(name = "ssl", ...)]` attributes fire
// and `libcrypto.a` / `libssl.a` get linked. Required for
// [`crate::externs::crypto`] handlers that call `SHA256` / `HMAC`
// / `EVP_*` directly. No Rust-path uses; pure link metadata.
use boring_sys as _;

mod abi;
mod error;
mod externs;
mod interpreter;
mod intrinsics;
mod ops;
mod reactor;
mod scheduler;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::{EnumPayload, Value};
