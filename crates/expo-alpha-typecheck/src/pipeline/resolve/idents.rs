//! Bare identifier and `self` resolution.

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::{Identifier, LocalId, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::GlobalKind;

use super::ctx::Resolver;

/// Resolve a bare identifier expression. Locals win first; package-
/// level constants resolve through a global lookup so an
/// `EARTH_RADIUS` reference at a use site stamps `Resolution::Global`
/// and returns the constant's stamped type. Functions and type names
/// don't flow as first-class values, so a non-constant global hit
/// still falls through to the unknown-identifier diagnostic. (The
/// static-method receiver and `Type.method(...)` call paths each
/// handle struct-name resolution directly so they don't go through
/// this helper.)
pub(super) fn resolve_ident(
    name: &str,
    resolution: &mut Resolution,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup(name) {
        *resolution = Resolution::Local(local_id);
        return ty.clone();
    }
    let global_id = Identifier::new(resolver.package, vec![name.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&global_id)
        && let GlobalKind::Constant(Some(def)) = &entry.kind
    {
        *resolution = Resolution::Global(id);
        return def.ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        format!("unknown identifier `{name}` in this scope"),
        span,
    ));
    ResolvedType::unresolved()
}

/// Resolve a `self` keyword expression. `self` is bound by the
/// enclosing instance method's `Param::Self_`, which the walker
/// seeds into the [`Resolver`]'s [`LocalScope`] under the name
/// `"self"`; a hit returns the receiver's struct type and stamps the
/// AST node's `local_id` slot so IR lower can read the slot through
/// the same `LocalRead` path body-declared locals use. A miss surfaces
/// as a diagnostic — `self` outside an instance method is invalid.
///
/// Note: `expr.resolution` keeps the receiver's *struct type* (not a
/// `Resolution::Local`); the `local_id` slot is the binding info,
/// the resolution slot is the static type. Same split as `ExprKind::Ident`,
/// where the inner `resolution` names the binding and the outer
/// `expr.resolution` carries the type.
///
/// [`LocalScope`]: crate::pipeline::local_scope::LocalScope
pub(super) fn resolve_self(
    local_id_slot: &mut Option<LocalId>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup("self") {
        *local_id_slot = Some(local_id);
        return ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        "`self` is only valid inside instance methods",
        span,
    ));
    ResolvedType::unresolved()
}
