//! Sealed-AST typechecker for the [`COMPILER-NORTHSTAR.md`] pipeline.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Single public entry point: [`check_program`]. It runs every
//! sub-pass internally and returns a sealed [`CheckedProgram`] on
//! success or a [`CheckFailure`] on failure. Diagnostics flow through
//! the shared `koja_ast::ast::Diagnostic` vocabulary. Seal violations
//! panic (compiler bugs, not user errors).
//!
//! Project-mode files keep their function items on `File.items`.
//! Script-mode files keep their top-level statements on `File.body`.
//! Both shapes share the same sub-passes. There is no synthetic
//! `fn main` wrapper.
//!
//! # Module layout
//!
//! - [`error`]: user-actionable failure type ([`CheckFailure`]).
//! - [`pipeline`]: ordered synthesis, binding, checking, and sealing.
//! - [`program`]: public entry point and program-level types.
//! - [`registry`]: [`GlobalRegistry`] of decls + diagnostic-friendly
//!   [`format_registry`] rendering.

mod error;
mod pipeline;
mod program;
mod registry;

pub use error::CheckFailure;
pub use koja_ast::coercion::{LiteralCoercion, NumericLiteralWidth};
pub use pipeline::{Substitution, peel_alias, substitute};
pub use program::{CheckedPackage, CheckedProgram, check_program};
pub use registry::{
    Candidate, CandidateDetail, CandidateKind, ConstantDefinition, Dispatch, EnumDefinition,
    FunctionSignature, GlobalKind, GlobalRegistry, KEYWORDS, ProtocolDefinition, RegistryEntry,
    ResolvedEnumVariant, ResolvedParam, ResolvedProtocolMethod, ResolvedStructField,
    ResolvedVariantData, StructDefinition, format_registry,
};
