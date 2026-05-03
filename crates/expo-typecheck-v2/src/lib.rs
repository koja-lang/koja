//! Sealed-AST typechecker for the [`COMPILER-NORTHSTAR.md`] pipeline.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! The single public entry point is [`check_program`]. It runs every
//! sub-pass internally (collect, resolve, seal) and hands back a
//! sealed [`CheckedProgram`] on success or a [`CheckFailure`] on
//! failure.
//!
//! Stage ownership: the parser owns parse diagnostics, this crate owns
//! typecheck diagnostics. If the input [`expo_parser::ParsedProgram`]
//! already carries error-severity parse diagnostics, [`check_program`]
//! halts immediately without contributing any diagnostics of its own;
//! the caller reads parse errors from `partial.iter()`.
//!
//! Diagnostics use the shared `expo_ast::ast::Diagnostic` vocabulary so
//! v2 outputs flow through the existing driver / LSP / shell sinks
//! without translation. Seal violations panic; they indicate compiler
//! bugs, not user errors.

mod collect;
mod labels;
mod program;
mod registry;
mod resolve;
mod seal;

pub use program::{CheckFailure, CheckedPackage, CheckedProgram, check_program};
pub use registry::{GlobalEntry, GlobalRegistry};
