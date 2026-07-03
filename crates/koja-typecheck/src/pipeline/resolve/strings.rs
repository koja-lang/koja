//! String literal resolution.

use koja_ast::ast::{Diagnostic, Expr, ExprKind, Literal, StringPart};
use koja_ast::identifier::ResolvedType;
use koja_ast::span::Span;

use super::calls::resolve_method_call_expr;
use super::ctx::Resolver;
use super::expr::resolve_expr;

const FORMAT_METHOD: &str = "format";

/// Resolve a string literal, pure or interpolated. Both shapes type
/// as `Global.String`. Each interpolation is resolved, and any part
/// whose inner expression isn't already `String`-typed is wrapped in
/// a `.format()` call so IR-lower sees a `String` value per part.
/// String-typed parts are left bare because `String.format()` adds
/// surrounding quotes (Debug rendering), which would corrupt the
/// user's interpolation `"hello #{name}"` into `hello "alice"`.
pub(super) fn resolve_string(
    parts: &mut [StringPart],
    _span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let string_type = resolver.registry.primitive("String");
    for part in parts.iter_mut() {
        if let StringPart::Interpolation { expr, .. } = part {
            resolve_interpolation(expr, &string_type, resolver, diagnostics);
        }
    }
    string_type
}

/// Resolve `expr`. If the result isn't already `String`, swap it
/// for a synthetic `expr.format()` MethodCall and dispatch through
/// the normal method-call resolver. Mirrors the in-place AST rewrite
/// pattern used by [`super::literals::carrier::dispatch_via_carrier`].
fn resolve_interpolation(
    expr: &mut Box<Expr>,
    string_type: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    resolve_expr(expr, resolver, diagnostics);
    if &expr.resolution == string_type {
        return;
    }
    wrap_in_format(expr);
    let formatted = resolve_method_call_expr(expr, None, resolver, diagnostics);
    expr.resolution = formatted;
}

/// Replace `*expr` in-place with `<original>.format()`, preserving
/// the original's span on the wrapping MethodCall.
fn wrap_in_format(expr: &mut Box<Expr>) {
    let span = expr.span;
    let placeholder = Expr::new(
        ExprKind::Literal {
            value: Literal::Unit,
        },
        span,
    );
    let original = std::mem::replace(expr.as_mut(), placeholder);
    expr.kind = ExprKind::MethodCall {
        args: Vec::new(),
        method: FORMAT_METHOD.to_string(),
        receiver: Box::new(original),
        type_args: Vec::new(),
    };
}
