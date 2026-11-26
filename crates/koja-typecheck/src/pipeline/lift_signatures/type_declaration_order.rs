//! The unions of type declarations in dependency order. A type
//! declaration is a `type X = ...` item.
//!
//! `canonical_union` reads the expansion of each type declaration
//! that is a member of a union. The type declarations resolve in file
//! order, so a union can read a type declaration that has no
//! expansion yet, or one whose own unions are not final. This module
//! finds what each type declaration reads, and builds its unions
//! again after the type declarations it reads.
//!
//! A type declaration `X` has a content edge to a type declaration
//! `Y` when the bare name `Y` is the whole right side of `X`, or a
//! member of a union at any depth of that right side. A name that is
//! a whole type argument, a tuple element, or a part of a function
//! type is not a content edge, because the type keeps the name and
//! never reads the expansion.
//!
//! A cycle of content edges has no order, and its unions have no
//! flat form. In `type T = Int | List<Float | T>`, the flat form of
//! `Float | T` holds `List<Float | T>`, which holds `Float | T`
//! again. Each type declaration on such a cycle gets a diagnostic and
//! a [`ResolvedType::Failed`] expansion. The fix in the source is a
//! type declaration for the inner union, as in
//! `type Item = Float | T`.

use std::collections::BTreeSet;

use koja_ast::ast::{Diagnostic, TypeExpr};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_graph::Graph;

use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::GlobalRegistry;

use super::type_aliases::{AliasSite, is_alias};

/// One place where the right side of a type declaration reads the
/// expansion of the type declaration `target`. `span` is the name at
/// that place.
pub(super) struct ContentEdge {
    pub(super) span: Span,
    pub(super) target: GlobalRegistryId,
}

/// Every content edge of the right side `type_expr`, in source order.
/// Each name must hold the stamp that `resolve_type_expr` gives it.
pub(super) fn content_edges(type_expr: &TypeExpr, registry: &GlobalRegistry) -> Vec<ContentEdge> {
    let mut edges = Vec::new();
    collect_peeled(type_expr, registry, &mut edges);
    edges
}

/// Collect from a position that `canonical_union` or `peel_alias`
/// reads through. The whole right side is such a position, and so is
/// each member of a union. The bare name of a type declaration there
/// is a content edge.
fn collect_peeled(type_expr: &TypeExpr, registry: &GlobalRegistry, edges: &mut Vec<ContentEdge>) {
    match type_expr {
        TypeExpr::Named {
            resolution: Resolution::Global(id),
            span,
            ..
        } if is_alias(*id, registry) => edges.push(ContentEdge {
            span: *span,
            target: *id,
        }),
        _ => collect_inner_unions(type_expr, registry, edges),
    }
}

/// Collect from a position that nothing reads through. A name there
/// is not a content edge, but a union inside the position has
/// members that are.
fn collect_inner_unions(
    type_expr: &TypeExpr,
    registry: &GlobalRegistry,
    edges: &mut Vec<ContentEdge>,
) {
    match type_expr {
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for param in params {
                collect_inner_unions(param, registry, edges);
            }
            collect_inner_unions(return_type, registry, edges);
        }
        TypeExpr::Generic { args, .. } => {
            for arg in args {
                collect_inner_unions(arg, registry, edges);
            }
        }
        TypeExpr::Named { .. } | TypeExpr::Self_ { .. } | TypeExpr::Unit { .. } => {}
        TypeExpr::Tuple { elements, .. } => {
            for element in elements {
                collect_inner_unions(element, registry, edges);
            }
        }
        TypeExpr::Union { types, .. } => {
            for member in types {
                collect_peeled(member, registry, edges);
            }
        }
    }
}

/// Diagnose each type declaration of `sites` that is on a cycle of
/// content edges, store [`ResolvedType::Failed`] as its expansion,
/// and return the other sites. A type declaration that only reads
/// one on a cycle gets no diagnostic, because the cycle is not in its
/// own definition.
pub(super) fn diagnose_content_cycles(
    sites: Vec<AliasSite>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<AliasSite> {
    let graph = content_graph(&sites);
    let stuck: BTreeSet<GlobalRegistryId> = graph.toposort().stuck.into_iter().collect();
    let mut ordered = Vec::with_capacity(sites.len());
    for site in sites {
        let Some(cycle) = graph.cycle_path(&site.id, &stuck) else {
            ordered.push(site);
            continue;
        };
        diagnostics.push(content_cycle_diagnostic(&site, &cycle, registry));
        registry.set_type_alias_definition_force(site.id, ResolvedType::Failed);
    }
    ordered
}

/// The diagnostic for `site`, which reads its own expansion through
/// the type declarations of `cycle`. An empty `cycle` is a type
/// declaration that reads itself. The span is the name in the right
/// side of `site` where the cycle starts.
fn content_cycle_diagnostic(
    site: &AliasSite,
    cycle: &[GlobalRegistryId],
    registry: &GlobalRegistry,
) -> Diagnostic {
    let first = cycle.first().copied().unwrap_or(site.id);
    let span = site
        .content_edges
        .iter()
        .find(|edge| edge.target == first)
        .map_or(site.span, |edge| edge.span);
    let through: Vec<String> = cycle
        .iter()
        .filter_map(|id| registry.get(*id))
        .map(|entry| format!("`{}`", entry.identifier))
        .collect();
    let message = if through.is_empty() {
        format!(
            "type `{}` is a member of a union in its own definition (cycle)",
            site.identifier
        )
    } else {
        format!(
            "type `{}` is a member of a union in its own definition through {} (cycle)",
            site.identifier,
            through.join(", ")
        )
    };
    Diagnostic::error_with_hint(
        message,
        "give the union inside the type argument a type declaration of its own",
        span,
    )
}

/// Build the unions of each type declaration of `sites` again, each
/// one after the type declarations it reads. The first build of a
/// union ran in file order. It could keep a later type declaration as
/// one member, or take the members that a type declaration had before
/// its own unions were final.
///
/// One pass is enough, because each type declaration that a union
/// reads has its final expansion before the union is built. `sites`
/// must hold no type declaration on a cycle of content edges.
pub(super) fn rebuild_in_dependency_order(sites: &[AliasSite], registry: &mut GlobalRegistry) {
    for id in content_graph(sites).toposort().ready {
        let Some(expansion) = registry.alias_expansion(id) else {
            continue;
        };
        let rebuilt = rebuild_unions(&expansion, registry);
        registry.set_type_alias_definition_force(id, rebuilt);
    }
}

/// The content edges between the type declarations of `sites`. An
/// edge to a type declaration outside `sites` is left out, because
/// that type declaration already has its final expansion.
fn content_graph(sites: &[AliasSite]) -> Graph<GlobalRegistryId> {
    let ids: BTreeSet<GlobalRegistryId> = sites.iter().map(|site| site.id).collect();
    let mut graph = Graph::new();
    for site in sites {
        graph.add_node(site.id);
        let targets = site.content_edges.iter().map(|edge| edge.target);
        for target in targets.filter(|target| ids.contains(target)) {
            graph.add_edge(site.id, target);
        }
    }
    graph
}

/// Build every union inside `ty` again through `canonical_union`.
/// [`substitute`] does this for each union that it walks, and a
/// substitution with no scope changes nothing else.
fn rebuild_unions(ty: &ResolvedType, registry: &GlobalRegistry) -> ResolvedType {
    substitute(ty, &Substitution::empty(), registry)
}
