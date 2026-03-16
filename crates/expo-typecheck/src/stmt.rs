//! Statement-level type checking.
//!
//! Handles assignments (with ownership tracking and borrow mutation checks),
//! compound assignments, return statements, break validation, and expression
//! statements.

use expo_ast::ast::*;

use crate::check::types_compatible;
use crate::context::{PassMode, TypeContext};
use crate::env::{CheckEnv, VarState};
use crate::expr::infer_expr;
use crate::types::{Type, resolve_type_expr};

/// Checks all statements in a body sequentially.
pub(crate) fn check_body(stmts: &[Statement], ctx: &mut TypeContext, ce: &mut CheckEnv) {
    for stmt in stmts {
        check_statement(stmt, ctx, ce);
    }
}

/// Type-checks a single statement, handling assignments, returns, breaks, and expressions.
pub(crate) fn check_statement(stmt: &Statement, ctx: &mut TypeContext, ce: &mut CheckEnv) {
    match stmt {
        Statement::Assignment {
            target,
            type_annotation,
            value,
            span,
        } => {
            let value_type = infer_expr(value, ctx, ce);

            let effective_type = if let Some(te) = type_annotation {
                let annotated = resolve_type_expr(te, ce.struct_names, ce.enum_names);
                if value_type.is_known()
                    && annotated.is_known()
                    && !types_compatible(&value_type, &annotated)
                {
                    ctx.error_with_hint(
                        format!(
                            "type mismatch: annotation is `{}` but value has type `{}`",
                            annotated.display(),
                            value_type.display()
                        ),
                        "ensure the annotation matches the expression type".into(),
                        *span,
                    );
                }
                annotated
            } else {
                value_type
            };

            if let Expr::Ident { name: src_name, .. } = value
                && let Some(src_info) = ce.env.get(src_name)
                && !src_info.ty.is_copy()
            {
                ce.mark_moved(src_name, *span);
            }

            match target {
                AssignTarget::LValue(lv) => {
                    if lv.segments.len() > 1
                        && lv.segments[0] == "self"
                        && ce.self_mode != PassMode::Move
                    {
                        ctx.error_with_hint(
                            format!(
                                "cannot mutate `{}` -- `self` is borrowed (read-only)",
                                lv.segments.join(".")
                            ),
                            "use `move self` and return the modified value to mutate".into(),
                            lv.span,
                        );
                        return;
                    }
                    if lv.segments.len() == 1 {
                        let name = &lv.segments[0];
                        if ctx.constants.contains_key(name) {
                            ctx.error_with_hint(
                                format!("cannot assign to constant `{}`", name),
                                "constants are immutable and cannot be reassigned".into(),
                                lv.span,
                            );
                            return;
                        }
                        if let Some(existing) = ce.get_type(name) {
                            let existing = existing.clone();
                            if existing.is_known()
                                && effective_type.is_known()
                                && !types_compatible(&existing, &effective_type)
                            {
                                ctx.error_with_hint(
                                    format!(
                                        "type mismatch: `{}` has type `{}`, cannot assign `{}`",
                                        name,
                                        existing.display(),
                                        effective_type.display()
                                    ),
                                    format!(
                                        "variable was first assigned as `{}`",
                                        existing.display()
                                    ),
                                    lv.span,
                                );
                            }
                            if let Some(info) = ce.env.get_mut(name) {
                                info.state = VarState::Live;
                            }
                        } else {
                            ce.insert_var(name.clone(), effective_type);
                        }
                    }
                }
                AssignTarget::Pattern(_) => {}
            }
        }
        Statement::Break { span, .. } => {
            if ce.loop_depth == 0 {
                ctx.error_with_hint(
                    "break outside of loop".to_string(),
                    "'break' can only be used inside 'loop' or 'while'".into(),
                    *span,
                );
            }
        }
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            let target_name = &target.segments[0];
            if ctx.constants.contains_key(target_name) {
                ctx.error_with_hint(
                    format!("cannot assign to constant `{}`", target_name),
                    "constants are immutable and cannot be reassigned".into(),
                    *span,
                );
                return;
            }
            let target_type = ce.get_type(target_name).cloned().unwrap_or_else(|| {
                ctx.error_with_hint(
                    format!("unknown variable `{}`", target_name),
                    "check the spelling or make sure it is defined before this line".into(),
                    *span,
                );
                Type::Error
            });
            let value_type = infer_expr(value, ctx, ce);
            if target_type.is_known() && value_type.is_known() && !target_type.is_numeric() {
                ctx.error(
                    format!(
                        "compound assignment requires numeric type, found `{}`",
                        target_type.display()
                    ),
                    *span,
                );
            }
        }
        Statement::Expr(expr) => {
            infer_expr(expr, ctx, ce);
        }
        Statement::Return { value, span } => {
            let actual = value
                .as_ref()
                .map(|v| infer_expr(v, ctx, ce))
                .unwrap_or(Type::Unit);
            if ce.return_type.is_known() && actual.is_known() && ce.return_type != actual {
                ctx.error_with_hint(
                    format!(
                        "return type mismatch: expected `{}`, found `{}`",
                        ce.return_type.display(),
                        actual.display()
                    ),
                    format!(
                        "function is declared to return `{}`",
                        ce.return_type.display()
                    ),
                    *span,
                );
            }
        }
    }
}
