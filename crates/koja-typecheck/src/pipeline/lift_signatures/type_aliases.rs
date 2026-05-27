//! `lift_type_aliases`: resolve each `type X = ...` RHS against the
//! registry's named decls and stamp the resolved [`ResolvedType`]
//! onto the alias entry. Runs after collect (alias entries exist as
//! `TypeAlias(None)`) and after protocol lift (so a future
//! protocol-typed alias works), and before struct / enum / function
//! lift (so their signatures can reference aliases).
//!
//! Cycle detection runs as a follow-up sweep: walk each alias's
//! expansion through the registry; a visit count exceeding the
//! number of registered aliases means we hit a cycle. Each cycle
//! diagnoses once and the offending alias's expansion is rewritten
//! to [`ResolvedType::unresolved`] so downstream peels short-circuit
//! cleanly.

use std::collections::HashSet;

use koja_ast::ast::{Diagnostic, Item};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::pipeline::aliases::collect_file_aliases;
use crate::program::CheckedPackage;
use crate::registry::{GlobalKind, GlobalRegistry};

use super::LiftScope;
use super::types::{TypeParamScope, resolve_type_expr};

/// Lift every `Item::TypeAlias` across every file: resolve the RHS,
/// stamp the resulting `ResolvedType` on the registered alias
/// entry. Then sweep for cycles and rewrite cycling aliases to
/// `ResolvedType::unresolved` so subsequent peels short-circuit.
pub(super) fn lift_type_aliases(
    packages: &[CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for pkg in packages {
        for file in &pkg.files {
            let aliases = collect_file_aliases(file);
            let scope = LiftScope {
                aliases: &aliases,
                package: &pkg.package,
                registry,
            };
            for item in &file.items {
                let Item::TypeAlias(alias) = item else {
                    continue;
                };
                let identifier = Identifier::new(scope.package, vec![alias.name.clone()]);
                let Some((id, _)) = scope.registry.lookup(&identifier) else {
                    continue;
                };
                let resolved = resolve_type_expr(
                    &alias.type_expr,
                    TypeParamScope::default(),
                    scope.resolution_scope(),
                    diagnostics,
                );
                scope.registry.set_type_alias_definition(id, resolved);
            }
        }
    }
    diagnose_alias_cycles(packages, registry, diagnostics);
}

/// For each alias entry, walk its expansion looking for itself.
/// On a cycle: emit one diagnostic and rewrite the expansion to
/// `ResolvedType::unresolved` so downstream peels return the
/// alias's `Named` head unchanged (no infinite recursion at peel
/// time).
fn diagnose_alias_cycles(
    packages: &[CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut alias_ids: Vec<(GlobalRegistryId, Identifier, koja_ast::span::Span)> = Vec::new();
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                let Item::TypeAlias(alias) = item else {
                    continue;
                };
                let identifier = Identifier::new(&pkg.package, vec![alias.name.clone()]);
                if let Some((id, entry)) = registry.lookup(&identifier) {
                    alias_ids.push((id, entry.identifier.clone(), alias.span));
                }
            }
        }
    }
    let mut cycling: Vec<(GlobalRegistryId, Identifier, koja_ast::span::Span)> = Vec::new();
    for (id, identifier, span) in &alias_ids {
        let mut seen: HashSet<GlobalRegistryId> = HashSet::new();
        if expansion_cycles(*id, registry, &mut seen) {
            cycling.push((*id, identifier.clone(), *span));
        }
    }
    for (id, identifier, span) in cycling {
        diagnostics.push(Diagnostic::error(
            format!("type alias `{identifier}` references itself (cycle)"),
            span,
        ));
        registry.set_type_alias_definition_force(id, ResolvedType::unresolved());
    }
}

/// Walk `id`'s expansion, returning true iff we revisit `id`. Only
/// traverses `Named { Global(other_alias) }` heads — aliases form
/// the cycle structure. Generic args, function param/ret types, and
/// union members are walked recursively.
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
            type_args
                .iter()
                .any(|arg| type_references_alias(arg, registry, seen))
        }
        ResolvedType::Named { type_args, .. } => type_args
            .iter()
            .any(|arg| type_references_alias(arg, registry, seen)),
        ResolvedType::Anonymous(koja_ast::identifier::AnonymousKind::Function { params, ret }) => {
            params
                .iter()
                .any(|p| type_references_alias(&p.ty, registry, seen))
                || type_references_alias(ret, registry, seen)
        }
        ResolvedType::Union(members) => members
            .iter()
            .any(|m| type_references_alias(m, registry, seen)),
        ResolvedType::Unresolved => false,
    }
}
