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
//!
//! # Module layout
//!
//! - [`error`] — user-actionable failure type ([`CheckFailure`]).
//! - [`labels`] — short, stable labels for AST shapes (used by every
//!   pass that mentions a node kind in a diagnostic).
//! - [`pipeline`] — sub-passes (collect, lift_signatures, resolve, seal).
//! - [`program`] — public entry point and program-level types.
//! - [`registry`] — [`GlobalRegistry`] of decls + diagnostic-friendly
//!   [`format_registry`] rendering.

mod error;
mod labels;
mod pipeline;
mod program;
mod registry;

pub use error::CheckFailure;
pub use program::{CheckedPackage, CheckedProgram, check_program};
pub use registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, InsertOutcome, RegistryEntry, ResolvedParam,
    format_registry,
};
