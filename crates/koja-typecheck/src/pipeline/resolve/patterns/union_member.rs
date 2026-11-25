//! A variant or struct pattern on a union subject.
//!
//! The pattern names a type, and that type picks the member the
//! pattern matches. Resolve wraps the pattern in a
//! [`Pattern::UnionMember`] that records the member, then resolves
//! the pattern against that member. `Parse.BadDigit(pos)` on a
//! `Net | Parse` subject becomes a `UnionMember` of `Parse` around
//! the variant pattern.
//!
//! Coverage and IR lowering read the wrapper, so neither of them
//! finds the member again. [`super::constructor`] is the mirror of
//! this step. It rewrites a node that only the parser makes.

use koja_ast::ast::{Diagnostic, Name, Pattern, name_texts};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use koja_ast::labels::pattern_span;

use super::super::ctx::Resolver;
use super::super::types::{lookup_type, peel_alias};
use super::resolve_pattern;
use crate::registry::{DisplayStyle, GlobalKind, display_type};

/// Resolve `pat` through a union subject. Returns `false` when the
/// step does not apply, and the caller then resolves `pat` the usual
/// way. The step applies when `pat` names an enum or a struct and
/// `subject_ty` is a union.
///
/// One member of the named type gets the wrapper. No member, or more
/// than one, is an error, and the pattern then resolves against no
/// subject so its bindings still enter scope.
pub(super) fn resolve_through_union(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let Some(named) = pattern_type(pat, subject_ty, resolver) else {
        return false;
    };
    let union_label = display_type(subject_ty, resolver.registry, DisplayStyle::Source);
    match named.members.as_slice() {
        [member] => {
            wrap(pat, member.clone());
            resolve_pattern(pat, subject_ty, resolver, diagnostics);
            return true;
        }
        [] => diagnostics.push(Diagnostic::error(
            format!(
                "type `{}` is not a member of union `{union_label}`",
                named.type_name,
            ),
            pattern_span(pat),
        )),
        [first, ..] => diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "`{}` names more than one member of union `{union_label}`",
                named.type_name,
            ),
            format!(
                "name one member with a typed binding, as in `value: {} ->`, and match on \
                 the binding",
                display_type(first, resolver.registry, DisplayStyle::Source),
            ),
            pattern_span(pat),
        )),
    }
    resolve_pattern(pat, &ResolvedType::Failed, resolver, diagnostics);
    true
}

/// The type that a pattern names, and the members of the subject
/// union that are that type.
struct PatternType {
    members: Vec<ResolvedType>,
    type_name: String,
}

/// The type that `pat` names, looked up among the members of the
/// union `subject_ty`. `None` when the step does not apply. That is
/// when `pat` names no type, when `subject_ty` is not a union, when
/// the path names nothing, and when the path names a type of the
/// wrong kind for the pattern. The usual resolution reports the last
/// two.
fn pattern_type(
    pat: &Pattern,
    subject_ty: &ResolvedType,
    resolver: &Resolver<'_>,
) -> Option<PatternType> {
    let type_path = named_type_path(pat)?;
    let peeled = peel_alias(subject_ty, resolver.registry);
    let members = peeled.as_union()?;
    let (type_id, entry) = lookup_type(&name_texts(type_path), resolver.resolution_scope())?;
    let kind_fits = match pat {
        Pattern::Struct { .. } => matches!(entry.kind, GlobalKind::Struct(_)),
        _ => matches!(entry.kind, GlobalKind::Enum(_)),
    };
    kind_fits.then(|| PatternType {
        members: members
            .iter()
            .filter(|member| is_type(member, type_id))
            .cloned()
            .collect(),
        type_name: entry.identifier.source_name(),
    })
}

/// The type path of a pattern that names an enum or a struct.
fn named_type_path(pat: &Pattern) -> Option<&[Name]> {
    match pat {
        Pattern::EnumStruct { type_path, .. }
        | Pattern::EnumTuple { type_path, .. }
        | Pattern::EnumUnit { type_path, .. }
        | Pattern::Struct { type_path, .. } => Some(type_path),
        _ => None,
    }
}

/// True when `member` is the registered type `type_id`, with any
/// type arguments.
fn is_type(member: &ResolvedType, type_id: GlobalRegistryId) -> bool {
    matches!(
        member,
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } if *id == type_id,
    )
}

/// Put `pat` inside a [`Pattern::UnionMember`] of `member_type`.
fn wrap(pat: &mut Pattern, member_type: ResolvedType) {
    let span = pattern_span(pat);
    let pattern = Box::new(std::mem::replace(pat, Pattern::Wildcard { span }));
    *pat = Pattern::UnionMember {
        member_type,
        pattern,
        span,
    };
}
