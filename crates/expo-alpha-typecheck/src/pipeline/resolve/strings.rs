//! String literal resolution.

use expo_ast::ast::{Diagnostic, StringPart};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr;

/// Resolve a string literal — pure or interpolated. Both shapes
/// type as `Global.String`. Interpolation segments are walked so
/// each inner expression's `resolution` is populated before
/// [`super::super::seal`] enforces the seal contract; IR-lower
/// folds the parts into a chain of `IRInstruction::Concat` over
/// per-part `String` values (the synthesizer wraps every
/// interpolated expression in `.format()` so it's already
/// `String`-typed by the time IR sees it).
pub(super) fn resolve_string(
    parts: &mut [StringPart],
    _span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    for part in parts.iter_mut() {
        if let StringPart::Interpolation { expr, .. } = part {
            resolve_expr(expr, resolver, diagnostics);
        }
    }
    resolver.registry.primitive("String")
}
