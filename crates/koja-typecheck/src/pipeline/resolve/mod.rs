//! The resolve sub-pass walks every body, populating `Resolution` on
//! identifier references and `Expr.resolution` on every expression.
//!
//! Type identity is registry-backed. Every primitive production goes
//! through [`crate::registry::GlobalRegistry::primitive`] so the
//! registry stays the single source of truth for what `Int` (etc.)
//! means.
//!
//! # Module layout
//!
//! - [`walker`]: top-down traversal, `resolve_file` then `resolve_function`
//!   then `resolve_statement`.
//! - [`statements`]: statement-level shapes, assignment decl /
//!   reassignment.
//! - [`expr`]: expression dispatch, `resolve_expr`.
//! - [`calls`]: bare and method-style call resolution.
//! - [`structs`]: struct-literal construction and field access.
//! - [`idents`]: bare identifier and `self` resolution.
//! - [`literals`]: literal-shaped expressions (list, map, binary).
//!   Shares carrier-protocol mechanics across protocol-aware
//!   literal families.
//! - [`strings`]: string literal resolution.
//! - [`control_flow`]: `if`, `cond`, the `?:` ternary, and `while`.
//! - [`error_channel`]: `try` / `fail` / `rescue` desugaring and
//!   `Result.Ok` auto-wrapping for `! E` functions.
//! - [`mod@assert`]: statement-position `assert` desugaring onto `if` and
//!   `fail Test.Failure.Assertion(...)`.
//! - [`ops`]: literal, binary, and unary type rules.
//! - [`return_type`]: trailing-expression-vs-declared-return checking.
//! - [`speculation`]: trial resolution with rollback, so sibling
//!   expressions (`match` arms, `==` operands) can hint each other.
//! - [`types`]: registry-backed [`ResolvedType`] predicates and
//!   diagnostic rendering.
//! - [`ctx`]: `Resolver`, the package + registry + scope bundle
//!   threaded through every recursion.
//!
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local
//! [`ResolvedType`]: koja_ast::identifier::ResolvedType

mod assert;
mod calls;
mod closures;
pub(crate) mod coercion;
mod control_flow;
mod ctx;
mod enums;
mod error_channel;
mod expr;
mod field_defaults;
mod for_loop;
mod idents;
mod inference;
pub(crate) mod literals;
mod match_expr;
mod ops;
mod paths;
mod patterns;
mod process;
mod return_type;
mod speculation;
mod statements;
mod strings;
mod structs;
pub(crate) mod types;
mod walker;

use koja_ast::ast::{AliasDecl, Diagnostic, Expr};
use koja_ast::identifier::ResolvedType;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::GlobalRegistry;

use ctx::ResolverEnv;
use expr::resolve_expr_with_expected;

pub(crate) use field_defaults::declaring_scope;
pub(crate) use idents::{constant_named_by_ident, constant_named_by_path};
pub(crate) use paths::static_dotted_path;
pub(crate) use walker::resolve_file;

/// Resolve `expr` with `expected` as the hint in a fresh scope made
/// of `package`, the given alias roster, and no locals. This is the
/// scope of a field default and of a constant value, both of which
/// resolve where they are declared and nowhere else.
pub(crate) fn resolve_in_declaring_scope(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    package: &str,
    aliases: &[AliasDecl],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let mut env = ResolverEnv {
        bound_overlay: None,
        file_aliases: aliases,
        package,
        registry,
    };
    let mut scope = LocalScope::new();
    let mut resolver = env.make_resolver(None, None, &[], &mut scope);
    resolve_expr_with_expected(expr, expected, &mut resolver, diagnostics);
    expr.resolution.clone()
}
