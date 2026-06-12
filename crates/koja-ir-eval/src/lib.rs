//! Tree-walking interpreter for sealed [`koja_ir::IRProgram`]s
//! and [`koja_ir::IRScript`]s — peer to
//! [`koja-ir-llvm`](../koja_ir_llvm/index.html) but
//! interpreting in-process instead of emitting native code.
//!
//! [`Interpreter::run_program`] / [`Interpreter::run_script`] return
//! the [`Value`] produced by the entry / body, or a [`RuntimeError`]
//! for the recoverable failure modes.
//!
//! Process scope: program mode runs the single `Process` entry
//! in-process — argv-shaped `List<String>` config, blocking
//! socket/TLS externs (no reactor), and `receive` over
//! OS-signal-delivered `Lifecycle` events plus `after` timeouts.
//! `spawn` and cross-process messaging are not implemented; they
//! surface [`RuntimeError::Unsupported`] with a `--backend=llvm`
//! hint. A full eval scheduler arrives as the second
//! implementation of the planned scheduler protocol (the native
//! runtime is the first).
//!
//! Hard contract: zero dependency on the v1 `koja-ir` / `koja-ir-eval`
//! path. Sealed `IRProgram` / `IRScript` from `koja-ir` are the
//! only inputs.

// Keep `koja-runtime`'s rlib in the link graph even if the direct
// Rust-path uses (e.g. [`crate::signals`]) ever go away: the
// `#[unsafe(no_mangle)] pub extern "C" fn`s referenced by
// [`crate::externs`] resolve at link time, and without a `use`
// cargo would skip the rlib and the C symbols would come up
// undefined.
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
mod signals;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::{EnumPayload, Value};
