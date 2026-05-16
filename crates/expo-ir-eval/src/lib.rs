//! Tree-walking interpreter for sealed [`expo_ir::IRProgram`]s
//! and [`expo_ir::IRScript`]s — peer to
//! [`expo-ir-llvm`](../expo_ir_llvm/index.html) but
//! interpreting in-process instead of emitting native code.
//!
//! [`Interpreter::run_program`] / [`Interpreter::run_script`] return
//! the [`Value`] produced by the entry / body, or a [`RuntimeError`]
//! for the recoverable failure modes.
//!
//! Hard contract: zero dependency on the v1 `expo-ir` / `expo-ir-eval`
//! path. Sealed `IRProgram` / `IRScript` from `expo-ir` are the
//! only inputs.

// Pull `expo-runtime`'s rlib into the link graph so the
// `#[unsafe(no_mangle)] pub extern "C" fn`s referenced by
// [`crate::externs`] resolve at link time. The crate has no Rust-
// path uses on its own, so without this import cargo would skip
// the rlib and the C symbols would come up undefined.
use expo_runtime as _;

mod error;
mod externs;
mod interpreter;
mod intrinsics;
mod ops;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::{EnumPayload, Value};
