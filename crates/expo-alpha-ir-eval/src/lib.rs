//! Tree-walking interpreter for sealed [`expo_alpha_ir::IRProgram`]s
//! and [`expo_alpha_ir::IRScript`]s — peer to
//! [`expo-alpha-ir-llvm`](../expo_alpha_ir_llvm/index.html) but
//! interpreting in-process instead of emitting native code.
//!
//! [`Interpreter::run_program`] / [`Interpreter::run_script`] return
//! the [`Value`] produced by the entry / body, or a [`RuntimeError`]
//! for the recoverable failure modes.
//!
//! Hard contract: zero dependency on the v1 `expo-ir` / `expo-ir-eval`
//! path. Sealed `IRProgram` / `IRScript` from `expo-alpha-ir` are the
//! only inputs.

mod error;
mod interpreter;
mod intrinsics;
mod ops;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::{EnumPayload, Value};
