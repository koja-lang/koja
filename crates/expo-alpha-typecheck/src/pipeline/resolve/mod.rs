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
//! - [`statements`] — statement-level shapes: assignment decl /
//!   reassignment.
//! - [`expr`] — expression dispatch: `resolve_expr`.
//! - [`calls`] — bare and method-style call resolution.
//! - [`structs`] — struct-literal construction and field access.
//! - [`idents`] — bare identifier and `self` resolution.
//! - [`strings`] — string literal resolution.
//! - [`control_flow`] — `if` / `unless` (Unit-typed; value-producing
//!   forms land with locals).
//! - [`ops`] — literal, binary, and unary type rules.
//! - [`return_type`] — trailing-expression-vs-declared-return checking.
//! - [`types`] — registry-backed [`ResolvedType`] predicates and
//!   diagnostic rendering.
//! - [`ctx`] — `Resolver`: the package + registry + scope bundle
//!   threaded through every recursion.
//!
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local
//! [`ResolvedType`]: expo_ast::identifier::ResolvedType

mod calls;
mod control_flow;
mod ctx;
mod enums;
mod expr;
mod idents;
mod ops;
mod return_type;
mod statements;
mod strings;
mod structs;
mod types;
mod walker;

pub(crate) use walker::resolve_file;
