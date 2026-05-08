//! Enum-flavored pattern resolution: `EnumUnit`, `EnumTuple`, and
//! `EnumStruct`. Owns the shared enum-lookup helpers
//! ([`lookup_pattern_enum`], [`build_enum_substitution`],
//! [`declared_shape_label`]) that the constructor shorthand
//! ([`super::constructor`]) and the exhaustiveness checker
//! ([`super::super::match_expr`]) re-use.
//!
//! The metadata-then-mutate split inside each shape ends the
//! immutable registry borrow before the resolver re-borrows itself
//! mutably to recurse into payload sub-patterns.

use expo_ast::ast::{Diagnostic, FieldPattern, Pattern};
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use expo_ast::labels::{pattern_kind_label, pattern_span};
use expo_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::structs::lookup_type;
use super::super::types::display_resolution;
use super::structs::{resolve_field_patterns_unbound, walk_field_patterns};
use super::{PatternCoverage, resolve_pattern};
use crate::pipeline::unify::substitute_resolved_type;
use crate::registry::{EnumDefinition, GlobalKind, ResolvedStructField, ResolvedVariantData};

pub(super) fn resolve_enum_unit_pattern(
    type_path: &[String],
    variant_name: &str,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let Some(target) = lookup_pattern_enum(type_path, subject_ty, span, resolver, diagnostics)
    else {
        return PatternCoverage::Other;
    };
    let Some((variant_index, variant)) = target.definition.lookup_variant(variant_name) else {
        diagnostics.push(Diagnostic::error(
            format!("`{}` has no variant `{variant_name}`", target.label),
            span,
        ));
        return PatternCoverage::Other;
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
    PatternCoverage::Variants(vec![variant_index])
}

pub(super) fn resolve_enum_tuple_pattern(
    type_path: &[String],
    variant_name: &str,
    elements: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let resolved = resolve_enum_tuple_metadata(
        type_path,
        variant_name,
        elements.len(),
        subject_ty,
        span,
        resolver,
        diagnostics,
    );
    let Some(metadata) = resolved else {
        resolve_enum_tuple_elements_unbound(elements, resolver, diagnostics);
        return PatternCoverage::Other;
    };
    for (element, element_ty) in elements.iter_mut().zip(metadata.element_types.iter()) {
        if !is_admitted_tuple_element(element) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only admits wildcard / binding patterns inside \
                     `{label}.{variant_name}` payloads (got `{kind}`)",
                    label = metadata.label,
                    kind = pattern_kind_label(element),
                ),
                pattern_span(element),
            ));
            continue;
        }
        resolve_pattern(element, element_ty, resolver, diagnostics);
    }
    PatternCoverage::Variants(vec![metadata.variant_index])
}

pub(super) fn resolve_enum_struct_pattern(
    type_path: &[String],
    variant_name: &str,
    fields: &mut [FieldPattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let resolved = resolve_enum_struct_metadata(
        type_path,
        variant_name,
        subject_ty,
        span,
        resolver,
        diagnostics,
    );
    let Some(metadata) = resolved else {
        resolve_field_patterns_unbound(fields, resolver, diagnostics);
        return PatternCoverage::Other;
    };
    let owner_label = format!("{}.{variant_name}", metadata.label);
    walk_field_patterns(
        &owner_label,
        fields,
        &metadata.declared,
        resolver,
        diagnostics,
    );
    PatternCoverage::Variants(vec![metadata.variant_index])
}

struct EnumTuplePatternMetadata {
    element_types: Vec<ResolvedType>,
    label: String,
    variant_index: u32,
}

/// Resolve everything needed to descend into the elements: the enum,
/// the variant, the substituted element types. Splits out so the
/// immutable borrow of the registry ends before
/// [`resolve_enum_tuple_pattern`] re-borrows the resolver mutably to
/// recurse into payload sub-patterns.
fn resolve_enum_tuple_metadata(
    type_path: &[String],
    variant_name: &str,
    supplied_arity: usize,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EnumTuplePatternMetadata> {
    let target = lookup_pattern_enum(type_path, subject_ty, span, resolver, diagnostics)?;
    let Some((variant_index, variant)) = target.definition.lookup_variant(variant_name) else {
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
    let element_types = declared
        .iter()
        .map(|ty| substitute_resolved_type(ty, &subst, target.enum_id))
        .collect();
    Some(EnumTuplePatternMetadata {
        element_types,
        label: target.label,
        variant_index,
    })
}

struct EnumStructPatternMetadata {
    declared: Vec<ResolvedStructField>,
    label: String,
    variant_index: u32,
}

/// Resolve the variant + substituted field roster for an
/// `EnumStruct` pattern. Splits out so the immutable registry
/// borrow ends before the per-field walk re-borrows the resolver
/// mutably to recurse into bindings.
fn resolve_enum_struct_metadata(
    type_path: &[String],
    variant_name: &str,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EnumStructPatternMetadata> {
    let target = lookup_pattern_enum(type_path, subject_ty, span, resolver, diagnostics)?;
    let Some((variant_index, variant)) = target.definition.lookup_variant(variant_name) else {
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
            name: field.name.clone(),
            ty: substitute_resolved_type(&field.ty, &subst, target.enum_id),
        })
        .collect();
    Some(EnumStructPatternMetadata {
        declared,
        label: target.label,
        variant_index,
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

pub(super) struct EnumPatternTarget<'a> {
    pub(super) definition: &'a EnumDefinition,
    pub(super) enum_id: GlobalRegistryId,
    pub(super) label: String,
}

/// Resolve `type_path` to the registered enum definition and
/// validate its head matches `subject_ty`'s head. Emits diagnostics
/// for unknown paths, non-enum heads, and subject mismatches.
pub(super) fn lookup_pattern_enum<'a>(
    type_path: &[String],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &'a Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<EnumPatternTarget<'a>> {
    let Some((enum_id, entry)) = lookup_type(type_path, resolver.package, resolver.registry) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the enum type `{}`",
                type_path.join("."),
            ),
            span,
        ));
        return None;
    };
    let GlobalKind::Enum(definition) = &entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot match against `{}`: it is a {}, not an enum",
                entry.identifier,
                entry.kind.label(),
            ),
            span,
        ));
        return None;
    };
    let Some(definition) = definition.as_ref() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: enum `{}` has no lifted definition",
                entry.identifier
            ),
            span,
        ));
        return None;
    };
    if subject_ty.is_resolved() && subject_ty.resolution != Resolution::Global(enum_id) {
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
/// instantiation. `Option<Int>.Some(x)` → subst maps `T` to `Int`.
pub(super) fn build_enum_substitution(
    enum_id: GlobalRegistryId,
    subject_ty: &ResolvedType,
) -> Vec<Option<ResolvedType>> {
    if subject_ty.resolution != Resolution::Global(enum_id) {
        return Vec::new();
    }
    subject_ty.type_args.iter().cloned().map(Some).collect()
}

pub(super) fn declared_shape_label(data: &ResolvedVariantData) -> &'static str {
    match data {
        ResolvedVariantData::Struct(_) => "a struct variant (named fields)",
        ResolvedVariantData::Tuple(_) => "a tuple variant (positional fields)",
        ResolvedVariantData::Unit => "a unit variant (no payload)",
    }
}

fn is_admitted_tuple_element(pat: &Pattern) -> bool {
    matches!(pat, Pattern::Binding { .. } | Pattern::Wildcard { .. })
}
