//! `if` / `unless` resolution. Both forms restrict the condition to
//! `Bool`, recursively resolve every body statement, and type the
//! whole expression as `Unit`.
//!
//! `else` is parser-recognized but not yet typecheckable —
//! [`resolve_if`] emits a feature-gap diagnostic and walks the branch
//! anyway so per-statement diagnostics still surface. Value-producing
//! `if` / `else` lands once the IR has block-result vocabulary.

use expo_ast::ast::{Diagnostic, Expr, Statement};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use crate::registry::GlobalRegistry;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::{display_resolution, is_primitive};
use super::walker::resolve_statement;

pub(super) fn resolve_if(
    condition: &mut Expr,
    then_body: &mut [Statement],
    else_body: Option<&mut [Statement]>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("if", condition, resolver.registry, diagnostics);
    for stmt in then_body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    if let Some(else_body) = else_body {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support `else` branches",
            span,
        ));
        for stmt in else_body.iter_mut() {
            resolve_statement(stmt, resolver, diagnostics);
        }
    }
    resolver.registry.primitive("Unit")
}

pub(super) fn resolve_unless(
    condition: &mut Expr,
    body: &mut [Statement],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("unless", condition, resolver.registry, diagnostics);
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    resolver.registry.primitive("Unit")
}

/// Diagnose a non-Bool condition on an `if` / `unless`. Skips the
/// check when the condition itself failed to resolve — its own
/// diagnostic is already in flight.
fn require_bool_condition(
    keyword: &str,
    condition: &Expr,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !condition.resolution.is_resolved() {
        return;
    }
    if !is_primitive(&condition.resolution, registry, "Bool") {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{keyword}` condition must be `Bool`, got `{}`",
                display_resolution(&condition.resolution, registry),
            ),
            condition.span,
        ));
    }
}
