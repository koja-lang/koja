//! Typed binding pattern resolution, for a pattern such as
//! `cat: Cat`.
//!
//! On a union subject, the type names one member or a union of
//! members. On any other subject, the type must be the subject type,
//! and the pattern then binds the whole subject.

use koja_ast::ast::{Diagnostic, TypeExpr};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::types::{peel_alias, types_equivalent};
use super::union_member::{member_index, not_a_member_error};
use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::registry::{DisplayStyle, GlobalRegistry, display_type};

/// The type that a typed binding names, checked against the subject.
/// `None` when the type does not resolve or does not fit the subject.
/// The error for each of those cases is in `diagnostics`.
pub(super) fn resolve_bound_type(
    type_expr: &mut TypeExpr,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let resolved = resolve_type_expr(
        type_expr,
        TypeParamScope::new(resolver.type_param_owners),
        resolver.resolution_scope(),
        diagnostics,
    );
    if !resolved.is_resolved() {
        return None;
    }
    let peeled_subject = peel_alias(subject_ty, resolver.registry);
    match &peeled_subject {
        ResolvedType::Anonymous(AnonymousKind::Union { members }) => {
            if let Err(missing) = named_members(&resolved, members, resolver.registry) {
                diagnostics.push(not_a_member_error(
                    &display_type(&missing, resolver.registry, DisplayStyle::Source),
                    subject_ty,
                    resolver.registry,
                    span,
                ));
                return None;
            }
        }
        _ if subject_ty.is_resolved()
            && !types_equivalent(subject_ty, &resolved, resolver.registry) =>
        {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typed-binding pattern requires a union subject, \
                     got `{}`",
                    display_type(subject_ty, resolver.registry, DisplayStyle::Source),
                ),
                span,
            ));
            return None;
        }
        _ => {}
    }
    Some(resolved)
}

/// The members of the subject union that a typed binding names, as
/// indexes into `members`. One index when the binding names one
/// member, and one index for each member when it names a union of
/// members. The error is the first type that the binding names and
/// the subject union does not hold.
pub(super) fn named_members(
    bound: &ResolvedType,
    members: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Result<Vec<usize>, ResolvedType> {
    if let Some(index) = member_index(members, bound, registry) {
        return Ok(vec![index]);
    }
    match peel_alias(bound, registry) {
        ResolvedType::Anonymous(AnonymousKind::Union {
            members: bound_members,
        }) => bound_members
            .into_iter()
            .map(|member| member_index(members, &member, registry).ok_or(member))
            .collect(),
        _ => Err(bound.clone()),
    }
}
