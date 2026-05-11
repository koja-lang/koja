//! Registry-backed [`ResolvedType`] predicates and rendering used
//! across the resolve sub-pass.
//!
//! "Primitive" here means a preloaded `Global.<name>` stdlib stub; see
//! [`GlobalRegistry::with_stdlib_stubs`]. Constructors for those
//! [`ResolvedType`]s live on the registry itself
//! ([`GlobalRegistry::primitive`]) since both `lift_signatures` and
//! `resolve` produce them.

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use expo_ast::span::Span;

use super::ctx::Callee;
use crate::pipeline::unify::Substitution;
use crate::registry::GlobalRegistry;

/// Does `ty` resolve to the preloaded `Global.<name>` stdlib stub?
pub(super) fn is_primitive(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return false;
    };
    if !type_args.is_empty() {
        return false;
    }
    let Some(entry) = registry.get(*id) else {
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

/// Two resolved types interchangeable at struct-field / call-arg /
/// return-type checks. Strict equality plus the `Int ≡ Int64` and
/// `Float ≡ Float64` aliases — `Int` and `Int64` map to the same
/// `IRType::Int64` and have the same v1 semantics; `Float` and
/// `Float64` similarly. Wider numeric coercion (`Int → Int32` etc.)
/// is a separate slice — see `ALPHA-ROADMAP.md`'s "numeric coercion
/// at struct-literal sites" entry.
pub(crate) fn types_equivalent(
    a: &ResolvedType,
    b: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    if a == b {
        return true;
    }
    is_primitive_pair(a, b, registry, "Int", "Int64")
        || is_primitive_pair(a, b, registry, "Float", "Float64")
}

/// `a` is `Global.<lhs>` and `b` is `Global.<rhs>` (or vice versa).
fn is_primitive_pair(
    a: &ResolvedType,
    b: &ResolvedType,
    registry: &GlobalRegistry,
    lhs: &str,
    rhs: &str,
) -> bool {
    (is_primitive(a, registry, lhs) && is_primitive(b, registry, rhs))
        || (is_primitive(a, registry, rhs) && is_primitive(b, registry, lhs))
}

/// Human-readable rendering of a [`ResolvedType`] for diagnostics:
/// dereferences `Global` heads through the registry so users see
/// `Int` rather than an opaque `#0`.
pub(super) fn display_resolution(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            let rendered_params = params
                .iter()
                .map(|p| display_resolution(&p.ty, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn ({rendered_params}) -> {}",
                display_resolution(ret, registry),
            )
        }
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => match registry.get(*id) {
            Some(entry) => entry.identifier.last().to_string(),
            None => format!("<id {id}>"),
        },
        ResolvedType::Named {
            resolution: Resolution::Local(local_id),
            ..
        } => format!("<local {local_id}>"),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => registry
            .type_param_name(*owner, *index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        ResolvedType::Named {
            resolution: Resolution::Unresolved,
            ..
        }
        | ResolvedType::Unresolved => "<unresolved>".to_string(),
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
    subst: &Substitution,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(bounds) = registry.type_param_bounds(callee.id) else {
        return;
    };
    if !subst.owns(callee.id) {
        return;
    }
    for (index, slot) in subst.slots(callee.id).iter().enumerate() {
        let Some(inferred) = slot else {
            continue;
        };
        let Some(param_bounds) = bounds.get(index) else {
            continue;
        };
        if param_bounds.is_empty() {
            continue;
        }
        let ResolvedType::Named {
            resolution: Resolution::Global(target_id),
            ..
        } = inferred
        else {
            continue;
        };
        let target_id = *target_id;
        for &protocol_id in param_bounds {
            if registry
                .lookup_conformance(target_id, protocol_id)
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
