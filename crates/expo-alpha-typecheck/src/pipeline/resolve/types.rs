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
/// return-type / type-parameter-binding / arm-join checks. Strict
/// structural equality plus the `Int ≡ Int64` and `Float ≡ Float64`
/// aliases applied recursively at every leaf — so
/// `Result<Int, String>` and `Result<Int64, String>` are equivalent,
/// `fn (Int) -> Int64` and `fn (Int64) -> Int` are equivalent, etc.
///
/// The alias arm is the early-bound stand-in for future union
/// membership: per `LANGUAGE.md`'s primitives table, `Int` is on
/// track to become an `Int8 | Int16 | Int32 | Int64` union, and
/// `Float` likewise. Today the registry keeps `Int` and `Int64` as
/// distinct `Identifier`s (so they remain distinct ids when one
/// becomes the union and the other its member); this function
/// papers over that with a hardcoded pair check. When unions land
/// the alias arm generalizes to a registry-backed
/// "is `a` a member of `b`'s union (or vice versa)?" check; every
/// caller of `types_equivalent` keeps working unchanged.
///
/// Wider numeric coercion (`Int → Int32` etc.) is a separate
/// concept — that's literal-fit coercion at type-equality sites,
/// handled by [`super::coercion::check_compatible`].
pub(crate) fn types_equivalent(
    a: &ResolvedType,
    b: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (
            ResolvedType::Named {
                resolution: a_head,
                type_args: a_args,
            },
            ResolvedType::Named {
                resolution: b_head,
                type_args: b_args,
            },
        ) => {
            if a_head == b_head && a_args.len() == b_args.len() {
                return a_args
                    .iter()
                    .zip(b_args)
                    .all(|(x, y)| types_equivalent(x, y, registry));
            }
            // Different heads: only the alias arm applies, and only
            // when both sides are bare leaves (no type-args).
            a_args.is_empty() && b_args.is_empty() && primitive_aliases(a, b, registry)
        }
        (
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: a_params,
                ret: a_ret,
            }),
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: b_params,
                ret: b_ret,
            }),
        ) => {
            a_params.len() == b_params.len()
                && a_params
                    .iter()
                    .zip(b_params)
                    .all(|(x, y)| x.mode == y.mode && types_equivalent(&x.ty, &y.ty, registry))
                && types_equivalent(a_ret, b_ret, registry)
        }
        _ => false,
    }
}

/// Top-level alias check: `a` is `Int` and `b` is `Int64` (or vice
/// versa), or the same for `Float` / `Float64`. Callers are
/// responsible for the structural recursion.
fn primitive_aliases(a: &ResolvedType, b: &ResolvedType, registry: &GlobalRegistry) -> bool {
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
