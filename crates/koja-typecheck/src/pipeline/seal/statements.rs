//! Statement-shape seal checks: assignment / compound-assign
//! target shapes plus per-statement recursion into expressions.
//! The resolver narrows assignment targets to [`koja_ast::ast::LValue`]s
//! with at least one segment and a stamped head `LocalId`; reaching
//! seal with anything else is an upstream bug.

use koja_ast::ast::{AssignTarget, LValue, Statement};
use koja_ast::span::Span;

use super::expressions::seal_expr;
use super::seal_panic;

pub(super) fn seal_statement(stmt: &Statement) {
    match stmt {
        Statement::Assignment {
            span,
            target,
            value,
            ..
        } => {
            seal_assign_target(target, *span);
            seal_expr(value);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            seal_compound_target(target, *span);
            seal_expr(value);
        }
        Statement::Expr(expr) => seal_expr(expr),
        Statement::Return {
            value: Some(value), ..
        } => seal_expr(value),
    }
}

/// Assignment targets must be [`AssignTarget::LValue`]s with at
/// least one segment and a stamped head `LocalId`. Multi-segment
/// targets (`p.x = …`) are a happy path past resolve — the head id
/// keys the IR's `LocalRead` / `LocalWrite`; the IR lower walks the
/// remaining segments itself. Pattern destructuring still bottoms
/// out on the resolver's feature-gap diagnostic and never reaches
/// seal.
fn seal_assign_target(target: &AssignTarget, statement_span: Span) {
    match target {
        AssignTarget::LValue(lvalue) => seal_lvalue_shape(lvalue, "assignment", statement_span),
        AssignTarget::Pattern(_) => seal_panic(
            "assignment target is a destructuring pattern; resolver rejects this shape",
            statement_span,
        ),
    }
}

/// Compound-assign targets are bare `LValue`s (the parser only
/// admits an `LValue` on the lhs of `+=` / `-=` / `*=` / `/=`). Past
/// resolve, a compound-assign target must carry at least one segment
/// *and* a stamped head `local_id`.
fn seal_compound_target(target: &LValue, statement_span: Span) {
    seal_lvalue_shape(target, "compound-assign", statement_span);
}

/// Shared seal for the two `LValue`-shaped assignment targets.
/// Single-segment and multi-segment are both happy paths; only
/// empty-segments and a missing head `local_id` indicate an
/// upstream invariant break.
fn seal_lvalue_shape(lvalue: &LValue, role: &str, statement_span: Span) {
    if lvalue.segments.is_empty() {
        seal_panic(
            &format!("{role} target has zero segments; parser should reject this"),
            lvalue.span,
        );
    }
    if lvalue.local_id.is_none() {
        seal_panic(
            &format!(
                "{role} target `{}` carries no LocalId; resolver should have stamped it on \
                 success or diagnosed otherwise",
                lvalue.segments.join("."),
            ),
            statement_span,
        );
    }
}
