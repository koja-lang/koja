//! Tree-walking interpreter for sealed [`expo_alpha_ir::IRProgram`]s
//! and [`expo_alpha_ir::IRScript`]s.
//!
//! Two public entry points, one shared walker:
//!
//! - [`Interpreter::run_program`] — project-mode source whose `fn
//!   main` lowered through `expo-alpha-ir::lower_program`.
//! - [`Interpreter::run_script`] — script-mode source whose
//!   top-level statements lowered through
//!   `expo-alpha-ir::lower_script`.
//!
//! Both return the [`Value`] produced by the entry / body (or a
//! [`RuntimeError`] for the runtime failure modes).
//!
//! POC scope mirrors `expo-alpha-ir` exactly: integer arithmetic on
//! constants, function calls, the boolean / comparison operators.
//! Real features (control flow, struct construction, pattern
//! matching, list/string ops) land here as the IR vocabulary grows.
//!
//! Hard contract: zero dependency on `expo-ir` or `expo-ir-eval` (the
//! legacy v1 path). The sealed `IRProgram` / `IRScript` from
//! `expo-alpha-ir` are the only inputs.

mod error;
mod interpreter;
mod value;

pub use error::RuntimeError;
pub use interpreter::Interpreter;
pub use value::Value;
