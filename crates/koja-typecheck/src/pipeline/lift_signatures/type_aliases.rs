//! `lift_type_aliases` resolves each `type X = ...` RHS against the
//! registry's named decls and stamps the resolved [`ResolvedType`]
//! onto the alias entry. Runs after collect (alias entries exist as
//! `TypeAlias(None)`) and after protocol lift (so an alias RHS can
//! name a protocol), and before struct / enum / function lift (so
//! their signatures can reference aliases).
//!
//! Cycle detection runs as a second sweep that walks each alias's
//! expansion through the registry. An alias that reaches itself with
//! no guard on the path is a cycle. Each cycle diagnoses once and the
//! offending alias's expansion is rewritten to
//! [`ResolvedType::Failed`] so downstream peels short-circuit cleanly.
//!
//! A third sweep diagnoses each alias that is a member of a union in
//! its own definition, as in `type T = Int | List<Float | T>`. A
//! fourth sweep builds the unions of each alias again, each alias
//! after the aliases it reads. Each alias resolves in file order, so
//! a union that names a later alias first holds that alias as one
//! member. `type B = Int | A` is then the same type before and after
//! `type A = String | Bool`, and so is `type B = List<Int | A>`.
//! [`super::type_declaration_order`] holds both sweeps.
//!
//! A guard is a type argument of `List`, `Map`, `Set`, or a generic
//! struct or enum. A value of a guarded type has a fixed size, so an
//! alias can name itself there, as in `type Tree = Int | List<Tree>`.
//! A fifth sweep records each alias that does so on the registry.
//! Such an alias must be a union, because the union is what the IR
//! gives a name of its own.

use std::collections::HashSet;

use koja_ast::ast::{Diagnostic, Item};
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::program::CheckedPackage;
use crate::registry::{BuiltinShape, GlobalKind, GlobalRegistry};

use super::for_each_item;
use super::type_declaration_order::{
    ContentEdge, content_edges, diagnose_content_cycles, rebuild_in_dependency_order,
};
use super::types::{TypeParamScope, resolve_type_expr};

/// Lift every `Item::TypeAlias` across every file. Resolve the RHS,
/// stamp the resulting `ResolvedType` on the registered alias
/// entry. Then sweep for cycles and rewrite cycling aliases to
/// `ResolvedType::Failed` so subsequent peels short-circuit. The
/// module doc describes the sweeps that follow.
pub(super) fn lift_type_aliases(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for_each_item(
        packages,
        registry,
        diagnostics,
        |item, scope, diagnostics| {
            let Item::TypeAlias(alias) = item else {
                return;
            };
            let identifier = Identifier::single(scope.package, alias.name.text.clone());
            let Some((id, _)) = scope.registry.lookup(&identifier) else {
                return;
            };
            let resolved = resolve_type_expr(
                &mut alias.type_expr,
                TypeParamScope::default(),
                scope.resolution_scope(),
                diagnostics,
            );
            scope.registry.set_type_alias_definition(id, resolved);
        },
    );
    let sites = collect_alias_sites(packages, registry);
    let sound = diagnose_alias_cycles(sites, registry, diagnostics);
    let ordered = diagnose_content_cycles(sound, registry, diagnostics);
    rebuild_in_dependency_order(&ordered, registry);
    record_recursive_aliases(&ordered, registry, diagnostics);
}

/// One registered alias's identity plus the declaration span for
/// attributing a cycle diagnostic. `content_edges` holds each place
/// where the right side reads the expansion of another alias.
pub(super) struct AliasSite {
    pub(super) content_edges: Vec<ContentEdge>,
    pub(super) id: GlobalRegistryId,
    pub(super) identifier: Identifier,
    pub(super) span: koja_ast::span::Span,
}

/// One site for each alias declaration of `packages`, in file order.
/// The resolve sweep must run first, because the content edges come
/// from the stamps it leaves on each right side.
fn collect_alias_sites(packages: &[CheckedPackage], registry: &GlobalRegistry) -> Vec<AliasSite> {
    let mut alias_sites: Vec<AliasSite> = Vec::new();
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                let Item::TypeAlias(alias) = item else {
                    continue;
                };
                let identifier = Identifier::single(&pkg.package, alias.name.text.clone());
                if let Some((id, entry)) = registry.lookup(&identifier) {
                    alias_sites.push(AliasSite {
                        content_edges: content_edges(&alias.type_expr, registry),
                        id,
                        identifier: entry.identifier.clone(),
                        span: alias.span,
                    });
                }
            }
        }
    }
    alias_sites
}

/// For each alias entry, walk its expansion looking for itself. On a
/// cycle, emit one diagnostic and rewrite the expansion to
/// `ResolvedType::Failed` so downstream peels return the alias's
/// `Named` head unchanged (no infinite recursion at peel time).
/// Returns the sites with no such cycle.
fn diagnose_alias_cycles(
    alias_sites: Vec<AliasSite>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<AliasSite> {
    // Detect every cycle before rewriting any expansion, so all
    // members of one cycle diagnose rather than just the first.
    let (cycling, sound): (Vec<AliasSite>, Vec<AliasSite>) =
        alias_sites.into_iter().partition(|site| {
            let mut seen: HashSet<GlobalRegistryId> = HashSet::new();
            expansion_cycles(site.id, registry, &mut seen)
        });
    for site in cycling {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("type alias `{}` references itself (cycle)", site.identifier),
            "an alias can name itself only in a type argument of `List`, `Map`, `Set`, \
             or a generic struct or enum",
            site.span,
        ));
        registry.set_type_alias_definition_force(site.id, ResolvedType::Failed);
    }
    sound
}

/// Record each alias that reaches itself through a guard. A union
/// goes on the registry as a recursive alias. An alias that only
/// names another alias needs no record, because it peels to that
/// alias. Any other shape is an error, and its expansion becomes
/// `ResolvedType::Failed`.
fn record_recursive_aliases(
    sites: &[AliasSite],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Classify every alias before rewriting any expansion, so each
    // alias of one cycle gets the same answer.
    let recursive: Vec<&AliasSite> = sites
        .iter()
        .filter(|site| refers_to_itself(site.id, registry))
        .collect();
    for site in recursive {
        match registry.alias_expansion(site.id) {
            Some(ResolvedType::Anonymous(AnonymousKind::Union { .. })) => {
                registry.mark_recursive_alias(site.id)
            }
            Some(expansion) if names_alias(&expansion, registry) => {}
            _ => {
                diagnostics.push(Diagnostic::error(
                    format!("recursive type alias `{}` must be a union", site.identifier),
                    site.span,
                ));
                registry.set_type_alias_definition_force(site.id, ResolvedType::Failed);
            }
        }
    }
}

/// Whether `ty` is the bare name of a type alias.
fn names_alias(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    matches!(
        ty,
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } if type_args.is_empty() && is_alias(*id, registry)
    )
}

pub(super) fn is_alias(id: GlobalRegistryId, registry: &GlobalRegistry) -> bool {
    matches!(
        registry.get(id).map(|entry| &entry.kind),
        Some(GlobalKind::TypeAlias(_))
    )
}

/// Whether the type `id` names takes the size of its type arguments
/// out of the size of the alias. `List`, `Map`, and `Set` hold their
/// elements on the heap. A generic struct or enum gets a box on the
/// field or payload that closes a cycle.
fn is_guard(id: GlobalRegistryId, registry: &GlobalRegistry) -> bool {
    matches!(
        registry.builtin_shape(id),
        Some(BuiltinShape::List | BuiltinShape::Map | BuiltinShape::Set)
    ) || matches!(
        registry.get(id).map(|entry| &entry.kind),
        Some(GlobalKind::Enum(_) | GlobalKind::Struct(_))
    )
}

/// Whether the expansion of alias `id` names `id` again at any depth,
/// with or without a guard on the path.
fn refers_to_itself(id: GlobalRegistryId, registry: &GlobalRegistry) -> bool {
    let Some(expansion) = registry.alias_expansion(id) else {
        return false;
    };
    let mut seen: HashSet<GlobalRegistryId> = HashSet::new();
    mentions_alias(&expansion, id, registry, &mut seen)
}

/// Whether `ty` names the alias `target` at any depth. The walk
/// enters the expansion of each other alias one time.
fn mentions_alias(
    ty: &ResolvedType,
    target: GlobalRegistryId,
    registry: &GlobalRegistry,
    seen: &mut HashSet<GlobalRegistryId>,
) -> bool {
    match ty {
        ResolvedType::Named {
            resolution: Resolution::Global(child),
            type_args,
        } => {
            if *child == target {
                return true;
            }
            let through_alias = seen.insert(*child)
                && registry
                    .alias_expansion(*child)
                    .is_some_and(|expansion| mentions_alias(&expansion, target, registry, seen));
            through_alias || any_mentions_alias(type_args, target, registry, seen)
        }
        ResolvedType::Named { type_args, .. } => {
            any_mentions_alias(type_args, target, registry, seen)
        }
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            any_mentions_alias(params, target, registry, seen)
                || mentions_alias(ret, target, registry, seen)
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            any_mentions_alias(elements, target, registry, seen)
        }
        ResolvedType::Anonymous(AnonymousKind::Union { members }) => {
            any_mentions_alias(members, target, registry, seen)
        }
        ResolvedType::Failed | ResolvedType::Unresolved => false,
    }
}

fn any_mentions_alias(
    types: &[ResolvedType],
    target: GlobalRegistryId,
    registry: &GlobalRegistry,
    seen: &mut HashSet<GlobalRegistryId>,
) -> bool {
    types
        .iter()
        .any(|ty| mentions_alias(ty, target, registry, seen))
}

/// Walk `id`'s expansion, returning true iff the walk revisits `id`. Only
/// traverses `Named { Global(other_alias) }` heads, since aliases form
/// the cycle structure. Generic args, function param/ret types, and
/// union members are walked recursively. The walk does not enter the
/// type arguments of a guard (see [`is_guard`]).
fn expansion_cycles(
    id: GlobalRegistryId,
    registry: &GlobalRegistry,
    seen: &mut HashSet<GlobalRegistryId>,
) -> bool {
    if !seen.insert(id) {
        return true;
    }
    let Some(expansion) = registry.alias_expansion(id) else {
        return false;
    };
    let cycles = type_references_alias(&expansion, registry, seen);
    seen.remove(&id);
    cycles
}

fn type_references_alias(
    ty: &ResolvedType,
    registry: &GlobalRegistry,
    seen: &mut HashSet<GlobalRegistryId>,
) -> bool {
    match ty {
        ResolvedType::Named {
            resolution: Resolution::Global(child),
            type_args,
        } => {
            if matches!(
                registry.get(*child).map(|e| &e.kind),
                Some(GlobalKind::TypeAlias(_))
            ) && expansion_cycles(*child, registry, seen)
            {
                return true;
            }
            if is_guard(*child, registry) {
                return false;
            }
            type_args
                .iter()
                .any(|arg| type_references_alias(arg, registry, seen))
        }
        ResolvedType::Named { type_args, .. } => type_args
            .iter()
            .any(|arg| type_references_alias(arg, registry, seen)),
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            params
                .iter()
                .any(|p| type_references_alias(p, registry, seen))
                || type_references_alias(ret, registry, seen)
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => elements
            .iter()
            .any(|e| type_references_alias(e, registry, seen)),
        ResolvedType::Anonymous(AnonymousKind::Union { members }) => members
            .iter()
            .any(|m| type_references_alias(m, registry, seen)),
        ResolvedType::Failed | ResolvedType::Unresolved => false,
    }
}
