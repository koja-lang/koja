//! Match-arm pattern resolution.
//!
//! Admits `Wildcard`, `Binding`, primitive `Literal`, `EnumUnit`,
//! `EnumTuple` / `EnumStruct` / `Struct` with arbitrary nested
//! pattern shapes in payload / field positions (literals, nested
//! enums, nested structs, or-alternatives), `Or` (alternatives
//! restricted to literal / EnumUnit, no bindings), and
//! `Constructor` shorthand (`Some(x)`, `None`, `Ok(x)`, ...).
//! `Constructor` rewrites in place to the corresponding `EnumTuple`
//! / `EnumUnit` after looking the variant up on the subject's enum,
//! so seal / generics-substitute / lowering never see the shape.
//! Every other shape diagnoses a feature gap. Coverage is not
//! computed here. [`super::match_expr`] runs [`usefulness`] over the
//! resolved arms once every pattern has its types.
//!
//! # Module layout
//!
//! - [`constructor`]: `Some(x)` / `None` / `Ok(x)` shorthand,
//!   rewritten in place to its qualified form.
//! - [`enums`]: `EnumUnit` / `EnumTuple` / `EnumStruct` shapes,
//!   plus the shared enum-lookup / generic-substitution helpers.
//! - [`structs`]: plain-struct destructure and the field-pattern
//!   walker shared with struct-shaped enum variants.
//! - [`or_pattern`]: `A | B | C` alternatives.
//! - [`union_member`]: a variant or struct pattern on a union
//!   subject, wrapped in place in a `UnionMember` that records the
//!   member it matches.
//! - [`literals`]: literal-vs-subject type checking and the
//!   canonical literal text that usefulness keys duplicate literals
//!   on.
//! - [`usefulness`]: exhaustiveness and reachability over the
//!   resolved arm patterns.

mod binary;
mod constructor;
mod enums;
mod literals;
mod or_pattern;
mod structs;
mod union_member;
mod usefulness;

use koja_ast::ast::{Diagnostic, Pattern, name_texts};
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::labels::pattern_span;
use koja_ast::span::Span;

use super::ctx::Resolver;
use super::types::{is_primitive, names_struct, peel_alias, types_equivalent};
use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::registry::{DisplayStyle, GlobalKind, GlobalRegistry, display_type};

pub(super) use usefulness::{DeconstructedPattern, SubjectCoverage};

pub(super) fn resolve_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Mirror of the value-side rewrite: `A.B { … }` becomes a `Struct`
    // pattern when the path names a struct.
    rewrite_dotted_struct_pattern(pat, resolver);
    if union_member::resolve_through_union(pat, subject_ty, resolver, diagnostics) {
        return;
    }
    match pat {
        Pattern::Binding { local_id, name, .. } => {
            let id = resolver.scope.declare(name.as_str(), subject_ty.clone());
            *local_id = Some(id);
        }
        Pattern::Constructor { .. } => {
            constructor::resolve_constructor_pattern(pat, subject_ty, resolver, diagnostics);
        }
        Pattern::EnumStruct {
            fields,
            span,
            type_path,
            type_resolution,
            variant,
        } => enums::resolve_enum_struct_pattern(
            &mut enums::EnumPatternHead {
                type_path,
                type_resolution,
                variant,
            },
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumTuple {
            elements,
            span,
            type_path,
            type_resolution,
            variant,
        } => enums::resolve_enum_tuple_pattern(
            &mut enums::EnumPatternHead {
                type_path,
                type_resolution,
                variant,
            },
            elements,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumUnit {
            span,
            type_path,
            type_resolution,
            variant,
        } => enums::resolve_enum_unit_pattern(
            &mut enums::EnumPatternHead {
                type_path,
                type_resolution,
                variant,
            },
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Literal {
            literal_coercion,
            span,
            value,
        } => literals::check_literal_matches_subject(
            value,
            literal_coercion,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Or { patterns, span } => {
            or_pattern::resolve_or_pattern(patterns, subject_ty, *span, resolver, diagnostics);
        }
        Pattern::Struct {
            fields,
            span,
            type_path,
            type_resolution,
        } => structs::resolve_struct_pattern(
            type_path,
            type_resolution,
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Wildcard { .. } => {}
        Pattern::Binary { segments, span } => {
            binary::resolve_binary_pattern(segments, subject_ty, *span, resolver, diagnostics);
        }
        Pattern::List { .. } => {
            diagnostics.push(Diagnostic::error(
                "typecheck does not yet support list patterns (blocked on IR \
                 list ops + a stable `List<T>` layout)",
                pattern_span(pat),
            ));
        }
        Pattern::Tuple { elements, span } => {
            resolve_tuple_pattern(elements, subject_ty, *span, resolver, diagnostics);
        }
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            type_expr,
            span,
        } => {
            let resolved = resolve_type_expr(
                type_expr,
                TypeParamScope::new(resolver.type_param_owners),
                resolver.resolution_scope(),
                diagnostics,
            );
            if !resolved.is_resolved() {
                return;
            }
            let peeled_subject = peel_alias(subject_ty, resolver.registry);
            match &peeled_subject {
                ResolvedType::Anonymous(AnonymousKind::Union { members }) => {
                    if let Err(missing) = named_members(&resolved, members, resolver.registry) {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "type `{}` is not a member of union `{}`",
                                display_type(&missing, resolver.registry, DisplayStyle::Source),
                                display_type(subject_ty, resolver.registry, DisplayStyle::Source),
                            ),
                            *span,
                        ));
                        return;
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
                        *span,
                    ));
                    return;
                }
                _ => {}
            }
            let id = resolver.scope.declare(name.as_str(), resolved.clone());
            *local_id = Some(id);
            *resolved_type = Some(resolved);
        }
        // The wrapper already holds the member, so its pattern
        // resolves against that member and not against the union.
        Pattern::UnionMember {
            member_type,
            pattern,
            ..
        } => resolve_pattern(pattern, member_type, resolver, diagnostics),
    }
}

/// The members of the subject union that a typed binding names, as
/// indexes into `members`. One index when the binding names one
/// member, and one index for each member when it names a union of
/// members. The error is the first type that the binding names and
/// the subject union does not hold.
fn named_members(
    bound: &ResolvedType,
    members: &[ResolvedType],
    registry: &GlobalRegistry,
) -> Result<Vec<usize>, ResolvedType> {
    let member_index = |ty: &ResolvedType| {
        members
            .iter()
            .position(|member| types_equivalent(member, ty, registry))
    };
    if let Some(index) = member_index(bound) {
        return Ok(vec![index]);
    }
    match peel_alias(bound, registry) {
        ResolvedType::Anonymous(AnonymousKind::Union {
            members: bound_members,
        }) => bound_members
            .into_iter()
            .map(|member| member_index(&member).ok_or(member))
            .collect(),
        _ => Err(bound.clone()),
    }
}

/// Resolve a `(a, b)` pattern against a tuple subject.
fn resolve_tuple_pattern(
    elements: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(element_types) =
        tuple_element_types(subject_ty, elements.len(), span, resolver, diagnostics)
    else {
        return;
    };
    for (pattern, element_ty) in elements.iter_mut().zip(&element_types) {
        resolve_pattern(pattern, element_ty, resolver, diagnostics);
    }
}

/// Element types of a tuple subject with `arity` elements. Diagnoses
/// a non-tuple subject or an arity mismatch and returns `None`.
/// Shared by match-arm tuple patterns and destructuring assignment.
pub(super) fn tuple_element_types(
    subject_ty: &ResolvedType,
    arity: usize,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<ResolvedType>> {
    let peeled = peel_alias(subject_ty, resolver.registry);
    let ResolvedType::Anonymous(AnonymousKind::Tuple {
        elements: element_types,
    }) = &peeled
    else {
        if subject_ty.is_resolved() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "tuple pattern requires a tuple subject, got `{}`",
                    display_type(subject_ty, resolver.registry, DisplayStyle::Source),
                ),
                span,
            ));
        }
        return None;
    };
    if arity != element_types.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "tuple pattern has {arity} elements but subject `{}` has {}",
                display_type(subject_ty, resolver.registry, DisplayStyle::Source),
                element_types.len(),
            ),
            span,
        ));
        return None;
    }
    Some(element_types.clone())
}

/// Rewrite an `EnumStruct` pattern whose path names a struct into a
/// `Struct` pattern. A no-op for real enum struct-variant patterns.
fn rewrite_dotted_struct_pattern(pat: &mut Pattern, resolver: &Resolver<'_>) {
    let Pattern::EnumStruct {
        type_path,
        variant,
        span,
        ..
    } = pat
    else {
        return;
    };
    let mut full = name_texts(type_path);
    full.push(variant.text.clone());
    if !names_struct(&full, resolver.resolution_scope()) {
        return;
    }
    let span = *span;
    let Pattern::EnumStruct {
        mut type_path,
        variant,
        fields,
        ..
    } = std::mem::replace(pat, Pattern::Wildcard { span })
    else {
        unreachable!("guarded by the match above");
    };
    type_path.push(variant);
    *pat = Pattern::Struct {
        type_path,
        type_resolution: Resolution::Unresolved,
        fields,
        span,
    };
}

/// True when `subject_ty` resolves to a primitive admitted as a
/// literal-comparable subject (`Bool` / `Int` / `Float` / `String`).
/// Patterns made entirely of catch-alls bypass this check at the
/// `resolve_match` level. Any subject type is fine when the only
/// patterns are wildcards / bindings.
pub(super) fn is_match_subject_primitive(
    subject_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    const PRIMITIVES: &[&str] = &[
        "Bool", "Float", "Float32", "Float64", "Int", "Int16", "Int32", "Int64", "Int8", "String",
        "UInt16", "UInt32", "UInt64", "UInt8",
    ];
    PRIMITIVES
        .iter()
        .any(|name| is_primitive(subject_ty, registry, name))
}

/// True when `subject_ty` peels to an enum or a union, the two
/// subject kinds whose arms may mix literal payload patterns with
/// structural ones.
pub(super) fn is_enum_or_union_subject(
    subject_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    match peel_alias(subject_ty, registry) {
        ResolvedType::Anonymous(AnonymousKind::Union { .. }) => true,
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => registry
            .get(id)
            .is_some_and(|entry| matches!(entry.kind, GlobalKind::Enum(_))),
        _ => false,
    }
}
