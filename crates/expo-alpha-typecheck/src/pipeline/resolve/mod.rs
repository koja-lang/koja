//! Resolve sub-pass: walk every body, populating `Resolution` on
//! identifier references and `Expr.resolution` on every expression.
//!
//! Today's scope covers integer arithmetic, boolean (`and`/`or`/`not`),
//! comparison (`== != < > <= >=`), and bare-identifier function calls.
//! Local references (including parameter uses) land with
//! [`Resolution::Local`] in a follow-up slice.
//!
//! Type identity is registry-backed: every primitive production goes
//! through [`crate::registry::GlobalRegistry::primitive`] so the
//! registry stays the single source of truth for what `Int` (etc.)
//! means.
//!
//! # Module layout
//!
//! - [`walker`] — top-down traversal: `resolve_file` → `resolve_function`
//!   → `resolve_statement`.
//! - [`expr`] — expression dispatch: `resolve_expr` plus call resolution.
//! - [`control_flow`] — `if` / `unless` (Unit-typed; value-producing
//!   forms land with locals).
//! - [`ops`] — literal, binary, and unary type rules.
//! - [`types`] — registry-backed [`ResolvedType`] predicates and
//!   diagnostic rendering.
//!
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local
//! [`ResolvedType`]: expo_ast::identifier::ResolvedType

mod control_flow;
mod expr;
mod ops;
mod types;
mod walker;

pub(crate) use walker::resolve_file;
