//! String literal resolution.

use expo_ast::ast::{Diagnostic, StringPart};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr;

/// Resolve a string literal. Pure (non-interpolated) strings type as
/// `Global.String`. Interpolation is a feature gap: the diagnostic
/// fires up front, but the interpolated expressions are still walked
/// so [`super::super::seal`] sees populated resolutions on the way
/// down — keeps gap reporting consistent with how other unsupported
/// shapes still recurse.
pub(super) fn resolve_string(
    parts: &mut [StringPart],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if parts
        .iter()
        .any(|part| matches!(part, StringPart::Interpolation { .. }))
    {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support string interpolation",
            span,
        ));
        for part in parts.iter_mut() {
            if let StringPart::Interpolation { expr, .. } = part {
                resolve_expr(expr, resolver, diagnostics);
            }
        }
        return ResolvedType::unresolved();
    }
    resolver.registry.primitive("String")
}
