//! Expression dispatch. Pattern-matches `ExprKind` and routes to the
//! per-shape resolver in [`super::calls`] (call / method-call),
//! [`super::structs`] (struct literal / field access),
//! [`super::idents`] (bare identifier / `self`), [`super::strings`]
//! (string literal), [`super::control_flow`] (`if` / `unless`), or
//! [`super::ops`] (literal / binary / unary). Every successful arm
//! returns the [`ResolvedType`] to stamp on `expr.resolution`.

use expo_ast::ast::{Diagnostic, Expr, ExprKind};
use expo_ast::identifier::ResolvedType;

use expo_ast::labels::expr_kind_label;

use super::calls::{resolve_call, resolve_method_call};
use super::control_flow::{resolve_cond, resolve_if, resolve_ternary, resolve_unless};
use super::ctx::Resolver;
use super::enums::resolve_enum_construction;
use super::idents::{resolve_ident, resolve_self};
use super::match_expr::resolve_match;
use super::ops::{binary_type, literal_type, unary_type};
use super::strings::resolve_string;
use super::structs::{resolve_field_access, resolve_struct_construction};

pub(super) fn resolve_expr(
    expr: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, resolver, diagnostics);
            resolve_expr(right, resolver, diagnostics);
            binary_type(*op, left, right, expr.span, resolver.registry, diagnostics)
        }
        ExprKind::Call {
            callee,
            args,
            type_args,
        } => resolve_call(callee, args, type_args, expr.span, resolver, diagnostics),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => resolve_enum_construction(type_path, variant, data, expr.span, resolver, diagnostics),
        ExprKind::FieldAccess { receiver, field } => {
            resolve_field_access(receiver, field, expr.span, resolver, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, resolver, diagnostics);
            inner.resolution.clone()
        }
        ExprKind::Ident { name, resolution } => {
            resolve_ident(name, resolution, expr.span, resolver, diagnostics)
        }
        ExprKind::Cond { arms, else_body } => resolve_cond(
            arms,
            else_body.as_deref_mut(),
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => resolve_if(
            condition,
            then_body,
            else_body.as_deref_mut(),
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Literal { value } => literal_type(value, resolver.registry),
        ExprKind::Match { subject, arms } => {
            resolve_match(subject, arms, expr.span, resolver, diagnostics)
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            type_args,
        } => resolve_method_call(
            receiver,
            method,
            args,
            type_args,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Self_ { local_id } => resolve_self(local_id, expr.span, resolver, diagnostics),
        ExprKind::String { parts, .. } => resolve_string(parts, expr.span, resolver, diagnostics),
        ExprKind::StructConstruction { type_path, fields } => {
            resolve_struct_construction(type_path, fields, expr.span, resolver, diagnostics)
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => resolve_ternary(
            condition,
            then_expr,
            else_expr,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, resolver, diagnostics);
            unary_type(*op, operand, expr.span, resolver.registry, diagnostics)
        }
        ExprKind::Unless { condition, body } => {
            resolve_unless(condition, body, resolver, diagnostics)
        }
        // Unsupported shapes diagnose and leave the expression
        // unresolved. Seal runs only on the success path, so an
        // `Unresolved` leaf here is harmless — diagnostics is non-empty
        // and `check_program` returns early.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };
    expr.resolution = ty;
}
