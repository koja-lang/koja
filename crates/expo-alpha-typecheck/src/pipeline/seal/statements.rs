//! Statement-shape seal checks: assignment / compound-assign
//! target shapes plus per-statement recursion into expressions.
//! The resolver narrows assignment targets to single-segment
//! [`expo_ast::ast::LValue`]s with stamped `LocalId`s; reaching
//! seal with anything else is an upstream bug.

use expo_ast::ast::{AssignTarget, LValue, Statement};
use expo_ast::span::Span;

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

/// Assignment targets must be single-segment [`AssignTarget::LValue`]s
/// — the resolver rejected pattern destructuring and dotted lvalues
/// upstream, so reaching seal with anything else is a compiler bug.
fn seal_assign_target(target: &AssignTarget, statement_span: Span) {
    match target {
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() != 1 {
                seal_panic(
                    &format!(
                        "assignment target has {} segments; resolver rejects multi-segment \
                         targets",
                        lvalue.segments.len(),
                    ),
                    lvalue.span,
                );
            }
        }
        AssignTarget::Pattern(_) => seal_panic(
            "assignment target is a destructuring pattern; resolver rejects this shape",
            statement_span,
        ),
    }
}

/// Compound-assign targets are bare `LValue`s (the AST shape only
/// admits the single-segment case as a happy-path; the resolver
/// rejects multi-segment forms and undeclared names). Past resolve,
/// a compound-assign target must carry both single-segment shape
/// *and* a stamped `local_id`.
fn seal_compound_target(target: &LValue, statement_span: Span) {
    if target.segments.len() != 1 {
        seal_panic(
            &format!(
                "compound-assign target has {} segments; resolver rejects multi-segment \
                 targets",
                target.segments.len(),
            ),
            target.span,
        );
    }
    if target.local_id.is_none() {
        seal_panic(
            &format!(
                "compound-assign target `{}` carries no LocalId; resolver should have \
                 stamped it on success or diagnosed otherwise",
                target.segments[0],
            ),
            statement_span,
        );
    }
}
