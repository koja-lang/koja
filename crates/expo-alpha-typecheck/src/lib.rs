//! Sealed-AST typechecker for the [`COMPILER-NORTHSTAR.md`] pipeline.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Single public entry point: [`check_program`]. It runs every
//! sub-pass internally and returns a sealed [`CheckedProgram`] on
//! success or a [`CheckFailure`] on failure. Diagnostics flow through
//! the shared `expo_ast::ast::Diagnostic` vocabulary; seal violations
//! panic (compiler bugs, not user errors).
//!
//! Project-mode files keep their function items on `File.items`;
//! script-mode files keep their top-level statements on `File.body`.
//! Both shapes share the same sub-passes — there is no synthetic
//! `fn main` wrapper.

mod collect;
mod labels;
mod lift_signatures;
mod program;
mod registry;
mod resolve;
mod seal;

pub use program::{CheckFailure, CheckedPackage, CheckedProgram, check_program};
pub use registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, InsertOutcome, RegistryEntry, ResolvedParam,
    format_registry,
};
