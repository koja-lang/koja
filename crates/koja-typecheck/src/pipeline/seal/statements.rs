//! Statement-shape seal checks: assignment / compound-assign
//! target shapes plus per-statement recursion into expressions.
//! Past resolve, every assignment target is an [`koja_ast::ast::LValue`]
//! with at least one segment and a stamped head `LocalId`. Reaching
//! seal with anything else is an upstream bug.

use koja_ast::ast::{LValue, Statement};
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
            seal_lvalue_shape(target, "assignment", *span);
            seal_expr(value);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            seal_lvalue_shape(target, "compound-assign", *span);
            seal_expr(value);
        }
        Statement::Expr(expr) => seal_expr(expr),
        Statement::Return {
            value: Some(value), ..
        } => seal_expr(value),
    }
}

/// Shared seal for assignment and compound-assign targets.
/// Single-segment and multi-segment (`p.x = …`) are both happy
/// paths: the head id keys the IR's `LocalRead` / `LocalWrite` and
/// the IR lower walks the remaining segments itself. Only
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
