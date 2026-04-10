//! Statement-level type checking.
//!
//! Handles assignments (with ownership tracking and borrow mutation checks),
//! compound assignments, return statements, break validation, and expression
//! statements.

use expo_ast::ast::*;

use crate::check::{record_coercion_if_needed, types_compatible};
use crate::context::{FunctionKind, PassMode, TypeContext};
use crate::env::{CheckEnv, VarState};
use crate::expr::{expr_span, infer_expr, resolve_receiver_base_name};
use crate::types::{Type, resolve_type_expr};

/// Checks all statements in a body sequentially.
pub(crate) fn check_body(stmts: &mut [Statement], ctx: &mut TypeContext, ce: &mut CheckEnv) {
    for stmt in stmts {
        check_statement(stmt, ctx, ce);
    }
}

/// Type-checks a single statement, handling assignments, returns, breaks, and expressions.
pub(crate) fn check_statement(stmt: &mut Statement, ctx: &mut TypeContext, ce: &mut CheckEnv) {
    match stmt {
        Statement::Assignment {
            target,
            type_annotation,
            value,
            span,
        } => {
            if let Some(te) = type_annotation {
                let mut hint = resolve_type_expr(te, ce.struct_names, ce.enum_names);
                ctx.resolve_type(&mut hint);
                ce.type_hint = Some(hint);
            } else {
                ce.type_hint = None;
            }

            let value_type = infer_expr(value, ctx, ce);
            ce.type_hint = None;

            let effective_type = if let Some(te) = type_annotation {
                let mut annotated = resolve_type_expr(te, ce.struct_names, ce.enum_names);
                ctx.resolve_type(&mut annotated);
                if value_type.is_known() && annotated.is_known() {
                    if !types_compatible(&value_type, &annotated) {
                        ctx.error_with_hint(
                            format!(
                                "type mismatch: annotation is `{}` but value has type `{}`",
                                annotated.display(),
                                value_type.display()
                            ),
                            "ensure the annotation matches the expression type".into(),
                            *span,
                        );
                    } else {
                        record_coercion_if_needed(&value_type, &annotated, expr_span(value), ctx);
                    }
                }
                annotated
            } else {
                value_type
            };

            if let ExprKind::Ident { name: src_name, .. } = &value.kind
                && let Some(src_info) = ce.env.get(src_name)
                && !src_info.ty.is_copy()
            {
                ce.mark_moved(src_name, *span);
            }

            match target {
                AssignTarget::LValue(lv) => {
                    if lv.segments.len() > 1
                        && lv.segments[0] == "self"
                        && ce.kind != FunctionKind::Instance(PassMode::Move)
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
            let root_type = ce.get_type(target_name).cloned().unwrap_or_else(|| {
                ctx.error_with_hint(
                    format!("unknown variable `{}`", target_name),
                    "check the spelling or make sure it is defined before this line".into(),
                    *span,
                );
                Type::Error
            });

            let target_type = if target.segments.len() > 1 {
                let mut ty = root_type;
                for seg in &target.segments[1..] {
                    ty = resolve_field_type(&ty, seg, ctx).unwrap_or(Type::Error);
                }
                ty
            } else {
                root_type
            };

            if target.segments.len() > 1
                && target.segments[0] == "self"
                && ce.kind != FunctionKind::Instance(PassMode::Move)
            {
                ctx.error_with_hint(
                    format!(
                        "cannot mutate `{}` -- `self` is borrowed (read-only)",
                        target.segments.join(".")
                    ),
                    "use `move self` and return the modified value to mutate".into(),
                    *span,
                );
                return;
            }

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

            if let ExprKind::MethodCall {
                receiver, method, ..
            } = &mut expr.kind
            {
                let span = expr.span;
                let is_static = matches!(
                    &receiver.kind,
                    ExprKind::Ident { name, .. } if ctx.resolve_name(name.as_str()).is_some()
                );
                if !is_static {
                    let recv_ty = if let ExprKind::Ident { name, .. } = &receiver.kind {
                        ce.get_type(name).cloned()
                    } else {
                        None
                    };
                    if let Some(recv_ty) = recv_ty {
                        let recv_ty = match &recv_ty {
                            Type::Indirect(inner) => inner.as_ref().clone(),
                            other => other.clone(),
                        };
                        let (base_id, _) = resolve_receiver_base_name(&recv_ty, ctx);
                        if let Some(id) = base_id
                            && let Some(ti) = ctx.get_type(&id)
                            && let Some(sig) = ti.functions.get(method.as_str())
                            && sig.kind == FunctionKind::Instance(PassMode::Move)
                            && sig.return_type != Type::Unit
                        {
                            ctx.warning_with_hint(
                                format!(
                                    "return value of `{method}` discarded; \
                                     method consumes its receiver via `move self`"
                                ),
                                format!("assign the result: `x = x.{method}(...)`"),
                                span,
                            );
                        }
                    }
                }
            }
        }
        Statement::Return { value, span } => {
            let actual = value
                .as_mut()
                .map(|v| infer_expr(v, ctx, ce))
                .unwrap_or(Type::Unit);
            if ce.return_type.is_known() && actual.is_known() {
                if !types_compatible(&actual, &ce.return_type) {
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
                } else if let Some(v) = value {
                    record_coercion_if_needed(&actual, &ce.return_type, expr_span(v), ctx);
                }
            }
        }
    }
}

/// Resolves the type of a struct field by name, returning `None` if the
/// type is not a struct or the field doesn't exist.
fn resolve_field_type(ty: &Type, field: &str, ctx: &TypeContext) -> Option<Type> {
    let struct_id = match ty {
        Type::Named { identifier, .. } => identifier,
        _ => return None,
    };
    let ti = ctx.get_type(struct_id)?;
    let fields = ti.fields()?;
    fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.clone())
}
