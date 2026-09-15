//! Enum-flavored pattern resolution: `EnumUnit`, `EnumTuple`, and
//! `EnumStruct`. Owns the shared enum-lookup helpers
//! ([`lookup_pattern_enum`], [`build_enum_substitution`],
//! [`declared_shape_label`]) that the constructor shorthand
//! ([`super::constructor`]) re-uses.
//!
//! The metadata-then-mutate split inside each shape ends the
//! immutable registry borrow before the resolver re-borrows itself
//! mutably to recurse into payload sub-patterns.

use koja_ast::ast::{Diagnostic, FieldPattern, Name, Pattern, name_texts, path_text};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::types::{display_resolution, lookup_type};
use super::resolve_pattern;
use super::structs::{resolve_field_patterns_unbound, walk_field_patterns};
use crate::pipeline::unify::{Substitution, substitute};
use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{EnumDefinition, GlobalKind, ResolvedStructField, ResolvedVariantData};

pub(super) fn resolve_enum_unit_pattern(
    head: &mut EnumPatternHead<'_>,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let variant_name = head.variant;
    let Some(target) = lookup_pattern_enum(head, subject_ty, span, resolver, diagnostics) else {
        return;
    };
    let Some((_, variant)) = target.definition.lookup_variant(variant_name.as_str()) else {
        diagnostics.push(Diagnostic::error(
            format!("`{}` has no variant `{variant_name}`", target.label),
            span,
        ));
        return;
    };
    if !matches!(variant.data, ResolvedVariantData::Unit) {
        diagnostics.push(Diagnostic::error(
            format!(
                "variant `{}.{variant_name}` is {}, not a unit variant",
                target.label,
                declared_shape_label(&variant.data),
            ),
            span,
        ));
    }
}

pub(super) fn resolve_enum_tuple_pattern(
    head: &mut EnumPatternHead<'_>,
    elements: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let element_types = resolve_enum_tuple_element_types(
        head,
        elements.len(),
        subject_ty,
        span,
        resolver,
        diagnostics,
    );
    let Some(element_types) = element_types else {
        resolve_enum_tuple_elements_unbound(elements, resolver, diagnostics);
        return;
    };
    for (element, element_ty) in elements.iter_mut().zip(element_types.iter()) {
        resolve_pattern(element, element_ty, resolver, diagnostics);
    }
}

pub(super) fn resolve_enum_struct_pattern(
    head: &mut EnumPatternHead<'_>,
    fields: &mut [FieldPattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let variant_name = head.variant;
    let resolved = resolve_enum_struct_metadata(head, subject_ty, span, resolver, diagnostics);
    let Some(metadata) = resolved else {
        resolve_field_patterns_unbound(fields, resolver, diagnostics);
        return;
    };
    let owner_label = format!("{}.{variant_name}", metadata.label);
    walk_field_patterns(
        &owner_label,
        fields,
        &metadata.declared,
        resolver,
        diagnostics,
    );
}

/// Resolve the enum and variant, then return the substituted element
/// types. Splits out so the immutable borrow of the registry ends
/// before [`resolve_enum_tuple_pattern`] re-borrows the resolver
/// mutably to recurse into payload sub-patterns.
fn resolve_enum_tuple_element_types(
    head: &mut EnumPatternHead<'_>,
    supplied_arity: usize,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<ResolvedType>> {
    let variant_name = head.variant;
    let target = lookup_pattern_enum(head, subject_ty, span, resolver, diagnostics)?;
    let Some((_, variant)) = target.definition.lookup_variant(variant_name.as_str()) else {
        diagnostics.push(Diagnostic::error(
            format!("`{}` has no variant `{variant_name}`", target.label),
            span,
        ));
        return None;
    };
    let ResolvedVariantData::Tuple(declared) = &variant.data else {
        diagnostics.push(Diagnostic::error(
            format!(
                "variant `{}.{variant_name}` is {}, not a tuple variant",
                target.label,
                declared_shape_label(&variant.data),
            ),
            span,
        ));
        return None;
    };
    if supplied_arity != declared.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "variant `{}.{variant_name}` expects {} positional element{}, got {}",
                target.label,
                declared.len(),
                if declared.len() == 1 { "" } else { "s" },
                supplied_arity,
            ),
            span,
        ));
    }
    let subst = build_enum_substitution(target.enum_id, subject_ty);
    Some(declared.iter().map(|ty| substitute(ty, &subst)).collect())
}

struct EnumStructPatternMetadata {
    declared: Vec<ResolvedStructField>,
    label: String,
}

/// Resolve the variant + substituted field roster for an
/// `EnumStruct` pattern. Splits out so the immutable registry
/// borrow ends before the per-field walk re-borrows the resolver
/// mutably to recurse into bindings.
fn resolve_enum_struct_metadata(
    head: &mut EnumPatternHead<'_>,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EnumStructPatternMetadata> {
    let variant_name = head.variant;
    let target = lookup_pattern_enum(head, subject_ty, span, resolver, diagnostics)?;
    let Some((_, variant)) = target.definition.lookup_variant(variant_name.as_str()) else {
        diagnostics.push(Diagnostic::error(
            format!("`{}` has no variant `{variant_name}`", target.label),
            span,
        ));
        return None;
    };
    let ResolvedVariantData::Struct(declared) = &variant.data else {
        diagnostics.push(Diagnostic::error(
            format!(
                "variant `{}.{variant_name}` is {}, not a struct variant",
                target.label,
                declared_shape_label(&variant.data),
            ),
            span,
        ));
        return None;
    };
    let subst = build_enum_substitution(target.enum_id, subject_ty);
    let declared = declared
        .iter()
        .map(|field| ResolvedStructField {
            default: field.default.clone(),
            name: field.name.clone(),
            ty: substitute(&field.ty, &subst),
        })
        .collect();
    Some(EnumStructPatternMetadata {
        declared,
        label: target.label,
    })
}

/// Walk every element pattern when the surrounding tuple variant
/// failed to resolve. Stamps `local_id` on bindings so seal sees a
/// well-formed AST even when an upstream diagnostic fired.
fn resolve_enum_tuple_elements_unbound(
    elements: &mut [Pattern],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for element in elements {
        resolve_pattern(element, &ResolvedType::unresolved(), resolver, diagnostics);
    }
}

/// The `Type.Variant` head of an enum pattern. `type_resolution` is
/// the pattern's stamp slot, written when `type_path` resolves.
pub(super) struct EnumPatternHead<'a> {
    pub(super) type_path: &'a [Name],
    pub(super) type_resolution: &'a mut Resolution,
    pub(super) variant: &'a Name,
}

pub(super) struct EnumPatternTarget<'a> {
    pub(super) definition: &'a EnumDefinition,
    pub(super) enum_id: GlobalRegistryId,
    pub(super) label: String,
}

/// Resolve `type_path` to the registered enum definition and
/// validate its head matches `subject_ty`'s head. Emits diagnostics
/// for unknown paths, non-enum heads, and subject mismatches. Stamps
/// `type_resolution` with the entry the path names as soon as the
/// lookup hits, so the pattern carries it even when a later check
/// fails.
pub(super) fn lookup_pattern_enum<'a>(
    head: &mut EnumPatternHead<'_>,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &'a Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EnumPatternTarget<'a>> {
    let type_path = head.type_path;
    let Some((enum_id, entry)) = lookup_type(&name_texts(type_path), resolver.resolution_scope())
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not recognize the enum type `{}`",
                path_text(type_path),
            ),
            span,
        ));
        return None;
    };
    *head.type_resolution = Resolution::Global(enum_id);
    check_reference_visibility(entry, resolver.package, span, diagnostics);
    let GlobalKind::Enum(definition) = &entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot match against `{}` because it is a {}, not an enum",
                entry.identifier,
                entry.kind.label(),
            ),
            span,
        ));
        return None;
    };
    let Some(definition) = definition.as_ref() else {
        diagnostics.push(Diagnostic::error(
            format!("enum `{}` has no lifted definition", entry.identifier),
            span,
        ));
        return None;
    };
    if subject_ty.is_resolved() && !is_named_global(subject_ty, enum_id) {
        diagnostics.push(Diagnostic::error(
            format!(
                "match arm pattern targets `{}`, but the subject has type `{}`",
                entry.identifier,
                display_resolution(subject_ty, resolver.registry),
            ),
            span,
        ));
    }
    Some(EnumPatternTarget {
        definition,
        enum_id,
        label: entry.identifier.to_string(),
    })
}

/// Build the type-arg substitution needed to view the enum's
/// declared payload types in the concrete subject's type-arg
/// instantiation. `Option<Int>.Some(x)` -> subst maps `T` to `Int`.
pub(super) fn build_enum_substitution(
    enum_id: GlobalRegistryId,
    subject_ty: &ResolvedType,
) -> Substitution {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = subject_ty
    else {
        return Substitution::empty();
    };
    if *id != enum_id {
        return Substitution::empty();
    }
    Substitution::from_args(enum_id, type_args)
}

fn is_named_global(ty: &ResolvedType, target: GlobalRegistryId) -> bool {
    matches!(
        ty,
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } if *id == target,
    )
}

pub(super) fn declared_shape_label(data: &ResolvedVariantData) -> &'static str {
    match data {
        ResolvedVariantData::Struct(_) => "a struct variant (named fields)",
        ResolvedVariantData::Tuple(_) => "a tuple variant (positional fields)",
        ResolvedVariantData::Unit => "a unit variant (no payload)",
    }
}
