//! Registry-backed [`ResolvedType`] predicates and rendering used
//! across the resolve sub-pass.
//!
//! "Primitive" here means a preloaded `Global.<name>` stdlib stub, see
//! [`GlobalRegistry::with_stdlib_stubs`]. Constructors for those
//! [`ResolvedType`]s live on the registry itself
//! ([`GlobalRegistry::primitive`]) since both `lift_signatures` and
//! `resolve` produce them.

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::ctx::Callee;
use crate::pipeline::aliases::rewrite_through_aliases;
use crate::pipeline::lift_signatures::ResolutionScope;
use crate::pipeline::unify::Substitution;
use crate::registry::{GlobalKind, GlobalRegistry, RegistryEntry};

/// Whether `path` resolves to a registered struct. Lets the resolver
/// tell a nested struct from a struct-shaped enum variant, since they parse
/// to the same node.
pub(super) fn names_struct(path: &[String], scope: ResolutionScope<'_>) -> bool {
    matches!(
        lookup_type(path, scope),
        Some((_, entry)) if matches!(entry.kind, GlobalKind::Struct(_))
    )
}

/// Resolve a (possibly multi-segment) type path against the
/// in-scope package, falling back to `Global` for stdlib stubs.
/// Cross-cutting registry helper used by every site that needs to
/// turn a parsed type path into a [`RegistryEntry`]: struct
/// construction, enum-variant construction, struct/enum patterns,
/// and static method dispatch. File aliases get first crack so
/// `alias Pkg.Type as Local` followed by `Local{...}` resolves
/// through to the target package.
///
/// Multi-segment paths (`Crypto.SHA256`, `HTTP.Headers`) resolve
/// directly against the registry, so callers can write the
/// qualified name without an `alias`. Same precedence as
/// [`super::super::lift_signatures::types::resolve_path_to_global`]:
/// alias rewrite first, then `<package>.<segments…>`, then for
/// multi-segment paths only the head-as-package interpretation
/// (`<path[0]>.<path[1..]>`, what `alias`-rewrite would
/// construct), and finally `Global.<segments…>`. Mismatches
/// return `None` (no diagnostic). Callers convert to errors with
/// their own message, since they have the kind context (struct vs enum
/// vs static call).
pub(crate) fn lookup_type<'r>(
    type_path: &[String],
    scope: ResolutionScope<'r>,
) -> Option<(GlobalRegistryId, &'r RegistryEntry)> {
    if let Some(target) =
        rewrite_through_aliases(scope.aliases, type_path, scope.package, scope.registry)
    {
        return scope.registry.lookup(&target);
    }
    if let Some(found) = scope
        .registry
        .lookup(&Identifier::new(scope.package, type_path.to_vec()))
    {
        return Some(found);
    }
    if type_path.len() >= 2
        && let Some(found) = scope
            .registry
            .lookup(&Identifier::new(&type_path[0], type_path[1..].to_vec()))
    {
        return Some(found);
    }
    scope
        .registry
        .lookup(&Identifier::new("Global", type_path.to_vec()))
}

/// Does `ty` resolve to the preloaded `Global.<name>` stdlib stub?
pub(crate) fn is_primitive(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
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

/// Does `ty` admit `+`, `-`, `*`, `/`? Used by binary-arithmetic
/// and compound-assign, mirrors the operand rule at
/// [`super::ops::binary_type`]'s `Add | Div | Mul | Sub` arm.
/// Accepts every numeric primitive. Cross-width pairing is rejected
/// at the binary-op site, not here.
pub(super) fn is_arithmetic_type(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    const NUMERIC: &[&str] = &[
        "Float", "Float32", "Int", "Int16", "Int32", "Int64", "Int8", "UInt16", "UInt32", "UInt64",
        "UInt8",
    ];
    NUMERIC.iter().any(|name| is_primitive(ty, registry, name))
}

/// Build a canonical `ResolvedType::Union` from `members`. Steps:
/// peel each member through aliases, flatten any `Union(_)` member
/// into the outer vec, sort by `display_resolution`, dedup by the
/// same key. Collapse 0 members to `Unit`, 1 member to itself, ≥2
/// to `Union(...)`. The sort+dedup makes `A | B` and `B | A` and
/// `A | A | B` all equal as `ResolvedType` values, so the existing
/// derive-`PartialEq` on `ResolvedType` compares unions correctly.
pub(crate) fn canonical_union(
    members: Vec<ResolvedType>,
    registry: &GlobalRegistry,
) -> ResolvedType {
    let mut flat = Vec::with_capacity(members.len());
    for member in members {
        match peel_alias(&member, registry) {
            ResolvedType::Union(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    flat.sort_by_key(|m| display_resolution(m, registry));
    flat.dedup_by_key(|m| display_resolution(m, registry));
    match flat.len() {
        0 => registry.primitive("Unit"),
        1 => flat.into_iter().next().expect("flat.len() == 1"),
        _ => ResolvedType::Union(flat),
    }
}

/// Follow `Named { Global(id) }` through `GlobalKind::TypeAlias`
/// expansions, returning the underlying type. Bare and non-alias
/// types pass through unchanged. Cycles are bounded by a small
/// recursion cap. `lift_type_aliases` rejects cycles up front, so
/// hitting the cap here is a registry invariant violation.
pub fn peel_alias(ty: &ResolvedType, registry: &GlobalRegistry) -> ResolvedType {
    peel_alias_capped(ty, registry, 32)
}

fn peel_alias_capped(ty: &ResolvedType, registry: &GlobalRegistry, fuel: usize) -> ResolvedType {
    if fuel == 0 {
        return ty.clone();
    }
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return ty.clone();
    };
    if !type_args.is_empty() {
        return ty.clone();
    }
    let Some(expansion) = registry.alias_expansion(*id) else {
        return ty.clone();
    };
    peel_alias_capped(&expansion, registry, fuel - 1)
}

/// Two resolved types interchangeable at struct-field / call-arg /
/// return-type / type-parameter-binding / arm-join checks. Strict
/// structural equality plus the `Int ≡ Int64` and `Float ≡ Float64`
/// aliases applied recursively at every leaf, so
/// `Result<Int, String>` and `Result<Int64, String>` are equivalent,
/// `fn (Int) -> Int64` and `fn (Int64) -> Int` are equivalent, etc.
///
/// The alias arm is the early-bound stand-in for future union
/// membership: per `LANGUAGE.md`'s primitives table, `Int` is on
/// track to become an `Int8 | Int16 | Int32 | Int64` union, and
/// `Float` likewise. Today the registry keeps `Int` and `Int64` as
/// distinct `Identifier`s (so they remain distinct ids when one
/// becomes the union and the other its member). This function
/// papers over that with a hardcoded pair check. When unions land
/// the alias arm generalizes to a registry-backed
/// "is `a` a member of `b`'s union (or vice versa)?" check, and every
/// caller of `types_equivalent` keeps working unchanged.
///
/// Wider numeric coercion (`Int -> Int32` etc.) is a separate
/// concept: that's literal-fit coercion at type-equality sites,
/// handled by [`super::coercion::check_compatible`].
pub(crate) fn types_equivalent(
    a: &ResolvedType,
    b: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    if a == b {
        return true;
    }
    let a_peeled = peel_alias(a, registry);
    let b_peeled = peel_alias(b, registry);
    if a_peeled == b_peeled {
        return true;
    }
    match (&a_peeled, &b_peeled) {
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
            a_args.is_empty()
                && b_args.is_empty()
                && primitive_aliases(&a_peeled, &b_peeled, registry)
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
                    .all(|(x, y)| types_equivalent(x, y, registry))
                && types_equivalent(a_ret, b_ret, registry)
        }
        (ResolvedType::Union(a_members), ResolvedType::Union(b_members)) => {
            a_members.len() == b_members.len()
                && a_members
                    .iter()
                    .zip(b_members)
                    .all(|(x, y)| types_equivalent(x, y, registry))
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
                .map(|p| display_resolution(p, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fn ({rendered_params}) -> {}",
                display_resolution(ret, registry),
            )
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            let rendered = elements
                .iter()
                .map(|e| display_resolution(e, registry))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({rendered})")
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
        ResolvedType::Union(members) => members
            .iter()
            .map(|m| display_resolution(m, registry))
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

/// Walk an inference substitution and emit one diagnostic per
/// inferred concrete type that fails to satisfy a bound on the
/// corresponding generic param. Substitution slots that are still
/// `None` (phantom) are skipped, the caller already emits a
/// "cannot infer" diagnostic for those. Inferred [`Resolution::TypeParam`]
/// substitutions (the bounded param being threaded into another
/// generic call) skip the head check. The bound is enforced where
/// the outer call's caller resolves.
///
/// Wording follows LANGUAGE.md §10's bound-enforcement message
/// verbatim. Keep this surface stable across slices that re-use it.
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
        for &protocol_id in param_bounds {
            let Some(satisfied) = protocol_bound_satisfied(inferred, protocol_id, registry) else {
                continue;
            };
            if satisfied {
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

/// Return a concrete bound decision for supported type heads. Tuples have
/// structural `Debug` and `Equality`. Other anonymous shapes remain deferred.
fn protocol_bound_satisfied(
    inferred: &ResolvedType,
    protocol_id: GlobalRegistryId,
    registry: &GlobalRegistry,
) -> Option<bool> {
    match peel_alias(inferred, registry) {
        ResolvedType::Named {
            resolution: Resolution::Global(target_id),
            ..
        } => Some(
            registry
                .lookup_conformance(target_id, protocol_id)
                .is_some(),
        ),
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
            Some(tuple_implements_protocol(protocol_id, registry))
        }
        _ => None,
    }
}

fn tuple_implements_protocol(protocol_id: GlobalRegistryId, registry: &GlobalRegistry) -> bool {
    registry.get(protocol_id).is_some_and(|entry| {
        entry.identifier.package() == "Global"
            && entry.identifier.path().len() == 1
            && matches!(entry.identifier.last(), "Debug" | "Equality")
    })
}
