//! Tree-walking interpreter for sealed [`koja_ir::IRProgram`]s
//! and [`koja_ir::IRScript`]s, the peer to
//! [`koja-ir-llvm`](../koja_ir_llvm/index.html) but
//! interpreting in-process instead of emitting native code.
//!
//! [`Interpreter::run_program`] / [`Interpreter::run_script`] return
//! the [`Value`] produced by the entry / body, or a [`RuntimeError`]
//! for the recoverable failure modes. Sealed `IRProgram` / `IRScript`
//! from `koja-ir` are the only inputs.
//!
//! Eval and the LLVM backend must agree on every observable result,
//! so where a helper here exists to match native output byte for
//! byte its doc names the native counterpart.
//!
//! Program mode boots the `Process` entry as PID 1 over the shared
//! cooperative scheduler ([`crate::scheduler`]), which implements the
//! `koja-runtime-core` protocol like the native adapter does. Process
//! intrinsics route through the core ([`crate::intrinsics`]) and I/O
//! parks on eval's own [`crate::reactor`].

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
// / `EVP_*` directly. No Rust-path uses, pure link metadata.
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

/// Whether the interpreter has a handler for the `@extern "C"`
/// symbol `link_name`. The driver asks before defaulting to the
/// interpreter so an FFI project falls through to LLVM instead of
/// failing at its first foreign call.
pub fn supports_extern(link_name: &str) -> bool {
    externs::SUPPORTED_EXTERNS.binary_search(&link_name).is_ok()
}
