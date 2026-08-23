//! Expression dispatch. Pattern-matches `ExprKind` and routes to the
//! per-shape resolver in [`super::calls`] (call / method-call),
//! [`super::structs`] (struct literal / field access),
//! [`super::idents`] (bare identifier / `self`), [`super::strings`]
//! (string literal), [`super::control_flow`] (`if` / `unless`), or
//! [`super::ops`] (literal / binary / unary). Every successful arm
//! returns the [`ResolvedType`] to stamp on `expr.resolution`.

use koja_ast::ast::{BinOp, Diagnostic, Expr, ExprKind};
use koja_ast::identifier::ResolvedType;
use koja_ast::labels::expr_kind_label;

use super::calls::{CallSite, resolve_call, resolve_method_call_expr};
use super::closures::{resolve_closure, resolve_short_closure};
use super::coercion::check_float_literal_finite;
use super::control_flow::{
    resolve_cond, resolve_if, resolve_loop, resolve_ternary, resolve_unless, resolve_while,
};
use super::ctx::Resolver;
use super::enums::resolve_enum_construction;
use super::error_channel::{resolve_rescue, resolve_try};
use super::idents::{resolve_ident, resolve_qualified_member, resolve_self};
use super::literals::{
    is_scalar_literal, resolve_binary_literal, resolve_list_literal, resolve_map_literal,
    resolve_scalar_literal, resolve_tuple_literal,
};
use super::match_expr::resolve_match;
use super::ops::{binary_type, resolve_equality_op_expr, unary_type};
use super::process::{resolve_receive, resolve_spawn};
use super::strings::resolve_string;
use super::structs::{
    resolve_field_access, resolve_struct_construction, rewrite_dotted_struct_construction,
};

/// Default entry point: resolves `expr` with no expected-type hint
/// from the surrounding context.
pub(super) fn resolve_expr(
    expr: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr_with_expected(expr, None, resolver, diagnostics);
}

/// Resolve `expr` with an optional expected-type hint. Closures,
/// constructors, control flow, and literal protocols consume it.
pub(super) fn resolve_expr_with_expected(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Pre-dispatch for AST-rewriting literal shapes. Scalar / List / Map
    // literals can rewrite the *outer* expression in place (e.g.
    // `s: Set<Int> = [1, 2]` becomes `Set.from_list([1, 2])` post-
    // resolve). The main `match &mut expr.kind` below holds a
    // borrow on `expr.kind`, which forbids replacing the kind from
    // inside an arm. Lifting these two cases out lets their
    // resolvers take `&mut Expr` and mutate the kind freely.
    if is_scalar_literal(expr) {
        let ty = resolve_scalar_literal(expr, expected, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    if matches!(expr.kind, ExprKind::List { .. }) {
        let ty = resolve_list_literal(expr, expected, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    if matches!(expr.kind, ExprKind::Map { .. }) {
        let ty = resolve_map_literal(expr, expected, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    // `MethodCall` is also lifted out: the field-as-callable
    // fallback rewrites `recv.field(args)` in place to
    // `Call { callee: FieldAccess(recv, field), args }`, which
    // requires `&mut Expr` access the main match's borrow on
    // `expr.kind` precludes.
    if matches!(expr.kind, ExprKind::MethodCall { .. }) {
        let ty = resolve_method_call_expr(expr, expected, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    // `==` / `!=` on user struct / enum operands rewrites to
    // `lhs.equals?(rhs)` (or `not lhs.equals?(rhs)`) before re-resolving.
    // Primitive operands stay on the [`binary_type`] fast path.
    // Same outer-expr-rewrite shape as List / Map / MethodCall
    // above.
    if matches!(
        expr.kind,
        ExprKind::Binary {
            op: BinOp::Eq | BinOp::NotEq,
            ..
        }
    ) {
        let ty = resolve_equality_op_expr(expr, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    // `try` / `rescue` desugar to `match` expressions over their
    // resolved subject, replacing the outer kind in place. Same
    // pre-dispatch shape as List / Map / MethodCall above.
    if matches!(expr.kind, ExprKind::Try { .. }) {
        let ty = resolve_try(expr, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    if matches!(expr.kind, ExprKind::Rescue { .. }) {
        let ty = resolve_rescue(expr, resolver, diagnostics);
        expr.resolution = ty;
        return;
    }
    // Rewrite `A.B { … }` to a struct construction when the path names
    // a struct. A no-op for real enum variants.
    rewrite_dotted_struct_construction(expr, resolver);
    if let Some(ty) = resolve_qualified_member(expr, resolver, diagnostics) {
        expr.resolution = ty;
        return;
    }
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, resolver, diagnostics);
            resolve_expr(right, resolver, diagnostics);
            binary_type(*op, left, right, expr.span, resolver.registry, diagnostics)
        }
        ExprKind::BinaryLiteral { segments } => {
            resolve_binary_literal(segments, expr.span, resolver, diagnostics)
        }
        ExprKind::Call {
            callee,
            args,
            type_args,
        } => resolve_call(
            callee,
            args,
            CallSite {
                out_type_args: type_args,
                expected,
                span: expr.span,
            },
            resolver,
            diagnostics,
        ),
        ExprKind::Closure {
            params,
            return_type,
            body,
        } => resolve_closure(
            params,
            return_type,
            body,
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => resolve_enum_construction(
            type_path,
            variant,
            data,
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        // Statement and arm-tail `fail`s are rewritten by the
        // statement walker before expression dispatch runs, so
        // reaching here means an illegal embedded position (a call
        // argument, a ternary branch, ...).
        ExprKind::Fail { value } => {
            resolve_expr(value, resolver, diagnostics);
            diagnostics.push(Diagnostic::error_with_hint(
                "`fail` exits the function and cannot be embedded in a larger expression",
                "`fail` goes anywhere `return` does: a statement of its own or a \
                 match arm tail",
                expr.span,
            ));
            ResolvedType::unresolved()
        }
        ExprKind::FieldAccess { receiver, field } => {
            resolve_field_access(receiver, field, expr.span, resolver, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr_with_expected(inner, expected, resolver, diagnostics);
            inner.resolution.clone()
        }
        ExprKind::Ident { name, resolution } => {
            resolve_ident(name, resolution, expr.span, resolver, diagnostics)
        }
        ExprKind::Cond { arms, else_body } => resolve_cond(
            arms,
            else_body.as_mut(),
            expected,
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
            else_body.as_mut(),
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Literal { value } => {
            check_float_literal_finite(value, expr.span, diagnostics);
            resolver.registry.literal_type(value)
        }
        ExprKind::Loop { body } => resolve_loop(body, resolver, diagnostics),
        ExprKind::Match { subject, arms } => {
            resolve_match(subject, arms, expected, expr.span, resolver, diagnostics)
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => resolve_receive(
            arms,
            after_timeout.as_deref_mut(),
            after_body,
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Self_ { local_id } => resolve_self(local_id, expr.span, resolver, diagnostics),
        ExprKind::ShortClosure { params, body } => {
            resolve_short_closure(params, body, expected, expr.span, resolver, diagnostics)
        }
        ExprKind::Spawn { expr: inner } => resolve_spawn(inner, expr.span, resolver, diagnostics),
        ExprKind::String { parts, .. } => resolve_string(parts, expr.span, resolver, diagnostics),
        ExprKind::StructConstruction { type_path, fields } => resolve_struct_construction(
            type_path,
            fields,
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => resolve_ternary(
            condition,
            then_expr,
            else_expr,
            expected,
            expr.span,
            resolver,
            diagnostics,
        ),
        ExprKind::Tuple { elements } => {
            resolve_tuple_literal(elements, expected, resolver, diagnostics)
        }
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, resolver, diagnostics);
            unary_type(*op, operand, expr.span, resolver.registry, diagnostics)
        }
        ExprKind::Unless { condition, body } => {
            resolve_unless(condition, body, resolver, diagnostics)
        }
        ExprKind::While { condition, body } => {
            resolve_while(condition, body, resolver, diagnostics)
        }
        // The body walker rewrites statement-position `for` before
        // expression dispatch. Reaching here means expression
        // position, which the pipeline does not support yet.
        ExprKind::For { .. } => {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support `for` in expression \
                 position (only statement-position `for` is supported)"
                    .to_string(),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
        // Unsupported shapes diagnose and leave the expression
        // unresolved. Seal runs only on the success path, so an
        // `Unresolved` leaf here is harmless: diagnostics is non-empty
        // and `check_program` returns early.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typecheck does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };
    expr.resolution = ty;
}
