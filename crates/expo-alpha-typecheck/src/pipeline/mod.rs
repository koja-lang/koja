//! Sub-passes of the alpha typecheck phase, run in order by
//! [`crate::check_program`].
//!
//! Each child module is one sub-pass with one entry point:
//!
//! - [`collect::collect_file`] — register every top-level decl in the
//!   [`crate::registry::GlobalRegistry`].
//! - [`lift_signatures::lift_signatures`] — resolve function param +
//!   return type expressions and stamp [`crate::registry::FunctionSignature`]s.
//! - [`resolve::resolve_file`] — walk every body, populating
//!   `Resolution` on identifiers and `Expr.resolution` on every node.
//! - [`seal::seal_ast`] — assert sealed-AST invariants. Panics on
//!   violation (compiler bug, not user error).
//!
//! The pipeline contract is "diagnostics or seal": if any diagnostic
//! is emitted, [`crate::check_program`] returns `Err` before `seal`
//! runs. `seal` therefore only ever sees fully-resolved trees.

pub(crate) mod collect;
pub(crate) mod lift_signatures;
pub(crate) mod local_scope;
pub(crate) mod resolve;
pub(crate) mod seal;
