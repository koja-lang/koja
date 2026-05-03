//! Tree-walking interpreter for sealed [`expo_alpha_ir::IRProgram`]s.
//!
//! The single public entry point is [`Interpreter`]. Construct one with
//! the sealed `IRProgram` from `expo-alpha-ir::lower_program`, call
//! [`Interpreter::run`], and you get back the [`Value`] produced by the
//! program's entry function (or a [`RuntimeError`] for the runtime
//! failure modes).
//!
//! POC scope mirrors `expo-alpha-ir` exactly: integer arithmetic on
//! constants. Real features (function calls, control flow, struct
//! construction, pattern matching, list/string ops) land here as the
//! IR vocabulary grows.
//!
//! Hard contract: zero dependency on `expo-ir` or `expo-ir-eval` (the
//! legacy v1 path). The sealed `IRProgram` from `expo-alpha-ir` is the
//! only input.

mod error;
mod interpreter;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::Value;
