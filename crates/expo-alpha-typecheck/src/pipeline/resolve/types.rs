//! Registry-backed [`ResolvedType`] predicates and rendering used
//! across the resolve sub-pass.
//!
//! "Primitive" here means a preloaded `Global.<name>` stdlib stub; see
//! [`GlobalRegistry::with_stdlib_stubs`]. Constructors for those
//! [`ResolvedType`]s live on the registry itself
//! ([`GlobalRegistry::primitive`]) since both `lift_signatures` and
//! `resolve` produce them.

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::span::Span;

use super::ctx::Callee;
use crate::registry::GlobalRegistry;

/// Does `ty` resolve to the preloaded `Global.<name>` stdlib stub?
pub(super) fn is_primitive(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    let Resolution::Global(id) = ty.resolution else {
        return false;
    };
    if !ty.type_args.is_empty() {
        return false;
    }
    let Some(entry) = registry.get(id) else {
        return false;
    };
    entry.identifier.is_in_global() && entry.identifier.last() == name
}

/// Does `ty` resolve to a primitive admitting `+`, `-`, `*`, `/`?
/// Used by both binary-arithmetic and compound-assign typechecking;
/// kept in sync with the operand rule at
/// [`super::ops::binary_type`]'s `Add | Div | Mul | Sub` arm.
pub(super) fn is_arithmetic_type(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    is_primitive(ty, registry, "Int") || is_primitive(ty, registry, "Float")
}

/// Human-readable rendering of a [`ResolvedType`] for diagnostics:
/// dereferences `Global` heads through the registry so users see
/// `Int` rather than an opaque `#0`.
pub(super) fn display_resolution(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty.resolution {
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.last().to_string(),
            None => format!("<id {id}>"),
        },
        Resolution::Local(local_id) => format!("<local {local_id}>"),
        Resolution::TypeParam { owner, index } => registry
            .type_param_name(owner, index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}

/// Walk an inference substitution and emit one diagnostic per
/// inferred concrete type that fails to satisfy a bound on the
/// corresponding generic param. Substitution slots that are still
/// `None` (phantom) are skipped — the caller already emits a
/// "cannot infer" diagnostic for those. Inferred [`Resolution::TypeParam`]
/// substitutions (the bounded param being threaded into another
/// generic call) skip the head check; the bound is enforced where
/// the outer call's caller resolves.
///
/// Wording follows LANGUAGE.md §10's bound-enforcement message
/// verbatim — keep this surface stable across slices that re-use it.
pub(super) fn verify_bounds(
    callee: Callee<'_>,
    subst: &[Option<ResolvedType>],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(bounds) = registry.type_param_bounds(callee.id) else {
        return;
    };
    for (index, slot) in subst.iter().enumerate() {
        let Some(inferred) = slot else {
            continue;
        };
        let Some(param_bounds) = bounds.get(index) else {
            continue;
        };
        if param_bounds.is_empty() {
            continue;
        }
        let Resolution::Global(target_id) = inferred.resolution else {
            continue;
        };
        for &protocol_id in param_bounds {
            if registry
                .lookup_protocol_impl(target_id, protocol_id)
                .is_some()
            {
                continue;
            }
            let bound_label = registry
                .get(protocol_id)
                .map(|e| e.identifier.last().to_string())
                .unwrap_or_else(|| format!("<id {protocol_id}>"));
            let param_name = callee
                .type_params
                .get(index)
                .map(String::as_str)
                .unwrap_or("?");
            diagnostics.push(Diagnostic::error(
                format!(
                    "type `{}` does not implement protocol `{bound_label}` \
                     (required by type parameter `{param_name}` in `{}`)",
                    display_resolution(inferred, registry),
                    callee.label,
                ),
                span,
            ));
        }
    }
}
