//! Match-arm pattern resolution.
//!
//! Admits `Wildcard`, `Binding`, primitive `Literal`, `EnumUnit`,
//! `EnumTuple` / `EnumStruct` (one-level — elements / fields
//! restricted to wildcard / binding), `Struct` (same restriction),
//! and `Or` (alternatives restricted to literal / EnumUnit, no
//! bindings). Every other shape diagnoses a feature gap. Returns
//! [`PatternCoverage`] so [`super::match_expr::resolve_match`] can
//! run the catch-all-or-exhaustiveness check without re-walking
//! the arm.

use std::collections::BTreeSet;

use expo_ast::ast::{Diagnostic, FieldPattern, Literal, Pattern};
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use expo_ast::labels::{pattern_kind_label, pattern_span};
use expo_ast::span::Span;

use crate::pipeline::unify::substitute_resolved_type;
use crate::registry::{
    EnumDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
};

use super::ctx::Resolver;
use super::ops::literal_type;
use super::structs::lookup_type;
use super::types::{display_resolution, is_primitive};

/// What a pattern admits at runtime. Drives the
/// catch-all-or-exhaustiveness rule in
/// [`super::match_expr::resolve_match`].
pub(super) enum PatternCoverage {
    /// Wildcard / binding — admits every value of the subject.
    CatchAll,
    /// `EnumUnit` / `EnumTuple` (or an `Or` of those) — admits
    /// exactly the listed variant tags.
    Variants(Vec<u32>),
    /// Literal patterns and `Or`s of literals. The arm fires for a
    /// specific runtime value but does not contribute to enum
    /// exhaustiveness; primitive subjects use the strict
    /// catch-all-required rule.
    Other,
}

pub(super) fn resolve_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    match pat {
        Pattern::Binding { local_id, name, .. } => {
            let id = resolver.scope.declare(name, subject_ty.clone());
            *local_id = Some(id);
            PatternCoverage::CatchAll
        }
        Pattern::EnumStruct {
            fields,
            span,
            type_path,
            variant,
            ..
        } => resolve_enum_struct_pattern(
            type_path,
            variant,
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
            variant,
            ..
        } => resolve_enum_tuple_pattern(
            type_path,
            variant,
            elements,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumUnit {
            span,
            type_path,
            variant,
            ..
        } => {
            resolve_enum_unit_pattern(type_path, variant, subject_ty, *span, resolver, diagnostics)
        }
        Pattern::Literal { span, value } => {
            check_literal_matches_subject(value, subject_ty, *span, resolver.registry, diagnostics);
            PatternCoverage::Other
        }
        Pattern::Or { patterns, span } => {
            resolve_or_pattern(patterns, subject_ty, *span, resolver, diagnostics)
        }
        Pattern::Struct {
            fields,
            span,
            type_path,
            ..
        } => resolve_struct_pattern(type_path, fields, subject_ty, *span, resolver, diagnostics),
        Pattern::Wildcard { .. } => PatternCoverage::CatchAll,
        Pattern::Binary { .. }
        | Pattern::Constructor { .. }
        | Pattern::List { .. }
        | Pattern::TypedBinding { .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support pattern `{}`",
                    pattern_kind_label(pat),
                ),
                pattern_span(pat),
            ));
            PatternCoverage::Other
        }
    }
}

fn resolve_enum_unit_pattern(
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

fn resolve_enum_tuple_pattern(
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

fn resolve_enum_struct_pattern(
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

fn resolve_struct_pattern(
    type_path: &[String],
    fields: &mut [FieldPattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let resolved = resolve_struct_metadata(type_path, subject_ty, span, resolver, diagnostics);
    let Some(metadata) = resolved else {
        resolve_field_patterns_unbound(fields, resolver, diagnostics);
        return PatternCoverage::CatchAll;
    };
    walk_field_patterns(
        &metadata.label,
        fields,
        &metadata.declared,
        resolver,
        diagnostics,
    );
    PatternCoverage::CatchAll
}

struct StructPatternMetadata {
    declared: Vec<ResolvedStructField>,
    label: String,
}

/// Resolve a plain-struct pattern's target and substituted field
/// roster. Mirrors [`resolve_enum_struct_metadata`] for the non-
/// variant case and substitutes generic type-args via
/// `subject_ty.type_args`, so `Bag<Int>{ item: x }` views `item` as
/// `Int`.
fn resolve_struct_metadata(
    type_path: &[String],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<StructPatternMetadata> {
    let Some((struct_id, entry)) = lookup_type(type_path, resolver.package, resolver.registry)
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the struct type `{}`",
                type_path.join("."),
            ),
            span,
        ));
        return None;
    };
    let GlobalKind::Struct(definition) = &entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot match against `{}`: it is a {}, not a struct",
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
                "internal: struct `{}` has no lifted definition",
                entry.identifier
            ),
            span,
        ));
        return None;
    };
    if subject_ty.is_resolved() && subject_ty.resolution != Resolution::Global(struct_id) {
        diagnostics.push(Diagnostic::error(
            format!(
                "match arm pattern targets `{}`, but the subject has type `{}`",
                entry.identifier,
                display_resolution(subject_ty, resolver.registry),
            ),
            span,
        ));
    }
    let subst: Vec<Option<ResolvedType>> = if subject_ty.resolution == Resolution::Global(struct_id)
    {
        subject_ty.type_args.iter().cloned().map(Some).collect()
    } else {
        Vec::new()
    };
    let declared = definition
        .fields
        .iter()
        .map(|field| ResolvedStructField {
            name: field.name.clone(),
            ty: substitute_resolved_type(&field.ty, &subst, struct_id),
        })
        .collect();
    Some(StructPatternMetadata {
        declared,
        label: entry.identifier.to_string(),
    })
}

/// Walk a `FieldPattern` list against a substituted declared
/// roster: lookup by name, gate on [`is_admitted_field_element`],
/// recurse into the binding / wildcard sub-pattern. Diagnoses
/// unknown fields and duplicate field-name patterns. Used by both
/// [`resolve_enum_struct_pattern`] and [`resolve_struct_pattern`].
fn walk_field_patterns(
    owner_label: &str,
    fields: &mut [FieldPattern],
    declared: &[ResolvedStructField],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen: Vec<bool> = vec![false; declared.len()];
    for field in fields {
        let lookup = declared
            .iter()
            .enumerate()
            .find(|(_, declared_field)| declared_field.name == field.name);
        let Some((index, declared_field)) = lookup else {
            diagnostics.push(Diagnostic::error(
                format!("`{owner_label}` has no field `{}`", field.name),
                field.span,
            ));
            resolve_pattern(
                &mut field.pattern,
                &ResolvedType::unresolved(),
                resolver,
                diagnostics,
            );
            continue;
        };
        if seen[index] {
            diagnostics.push(Diagnostic::error(
                format!("field `{}` of `{owner_label}` matched twice", field.name),
                field.span,
            ));
            resolve_pattern(
                &mut field.pattern,
                &declared_field.ty,
                resolver,
                diagnostics,
            );
            continue;
        }
        seen[index] = true;
        if !is_admitted_field_element(&field.pattern) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only admits wildcard / binding patterns inside \
                     `{owner_label}` fields (got `{}`)",
                    pattern_kind_label(&field.pattern),
                ),
                pattern_span(&field.pattern),
            ));
            continue;
        }
        resolve_pattern(
            &mut field.pattern,
            &declared_field.ty,
            resolver,
            diagnostics,
        );
    }
}

/// Walk every field pattern when the surrounding struct / variant
/// failed to resolve. Stamps `local_id` on bindings so seal sees a
/// well-formed AST even when an upstream diagnostic fired.
fn resolve_field_patterns_unbound(
    fields: &mut [FieldPattern],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields {
        resolve_pattern(
            &mut field.pattern,
            &ResolvedType::unresolved(),
            resolver,
            diagnostics,
        );
    }
}

fn resolve_or_pattern(
    patterns: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    if patterns.is_empty() {
        diagnostics.push(Diagnostic::error("or-pattern is empty", span));
        return PatternCoverage::Other;
    }
    let mut variant_tags: Vec<u32> = Vec::new();
    let mut all_literal = true;
    let mut all_enum_units = true;
    let mut seen_alt_literals: BTreeSet<String> = BTreeSet::new();
    let mut seen_alt_variants: BTreeSet<u32> = BTreeSet::new();
    for alternative in patterns.iter_mut() {
        if !is_admitted_or_alternative(alternative) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only admits literal / enum-unit alternatives in \
                     or-patterns (got `{}`)",
                    pattern_kind_label(alternative),
                ),
                pattern_span(alternative),
            ));
            all_literal = false;
            all_enum_units = false;
            continue;
        }
        let alt_span = pattern_span(alternative);
        let pre_literal = match alternative {
            Pattern::Literal { value, .. } => Some(literal_repr(value)),
            _ => None,
        };
        match resolve_pattern(alternative, subject_ty, resolver, diagnostics) {
            PatternCoverage::Variants(tags) => {
                all_literal = false;
                let mut all_dup = !tags.is_empty();
                for tag in &tags {
                    if !seen_alt_variants.insert(*tag) {
                        continue;
                    }
                    all_dup = false;
                }
                if all_dup {
                    diagnostics.push(Diagnostic::warning(
                        "or-pattern alternative is unreachable: already listed earlier in \
                         this or-pattern",
                        alt_span,
                    ));
                }
                variant_tags.extend(tags);
            }
            PatternCoverage::Other => {
                all_enum_units = false;
                if let Some(repr) = pre_literal
                    && !seen_alt_literals.insert(repr)
                {
                    diagnostics.push(Diagnostic::warning(
                        "or-pattern alternative is unreachable: already listed earlier in \
                         this or-pattern",
                        alt_span,
                    ));
                }
            }
            PatternCoverage::CatchAll => {
                // Only reachable via an unhandled future shape; the
                // single-test guard above already rejects bindings /
                // wildcards inside or-patterns.
                all_literal = false;
                all_enum_units = false;
            }
        }
    }
    if all_enum_units && !all_literal {
        PatternCoverage::Variants(variant_tags)
    } else {
        PatternCoverage::Other
    }
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

struct EnumPatternTarget<'a> {
    definition: &'a EnumDefinition,
    enum_id: GlobalRegistryId,
    label: String,
}

/// Resolve `type_path` to the registered enum definition and
/// validate its head matches `subject_ty`'s head. Emits diagnostics
/// for unknown paths, non-enum heads, and subject mismatches.
fn lookup_pattern_enum<'a>(
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
fn build_enum_substitution(
    enum_id: GlobalRegistryId,
    subject_ty: &ResolvedType,
) -> Vec<Option<ResolvedType>> {
    if subject_ty.resolution != Resolution::Global(enum_id) {
        return Vec::new();
    }
    subject_ty.type_args.iter().cloned().map(Some).collect()
}

fn is_admitted_field_element(pat: &Pattern) -> bool {
    matches!(pat, Pattern::Binding { .. } | Pattern::Wildcard { .. })
}

fn is_admitted_or_alternative(pat: &Pattern) -> bool {
    matches!(pat, Pattern::EnumUnit { .. } | Pattern::Literal { .. })
}

fn is_admitted_tuple_element(pat: &Pattern) -> bool {
    matches!(pat, Pattern::Binding { .. } | Pattern::Wildcard { .. })
}

fn declared_shape_label(data: &ResolvedVariantData) -> &'static str {
    match data {
        ResolvedVariantData::Struct(_) => "a struct variant (named fields)",
        ResolvedVariantData::Tuple(_) => "a tuple variant (positional fields)",
        ResolvedVariantData::Unit => "a unit variant (no payload)",
    }
}

/// Walk `pattern` and append a canonical string representation of
/// every `Literal` / `Or`-of-literal alternative it contains. Used
/// by [`super::match_expr::resolve_match`] to detect cross-arm
/// literal duplication (`1 -> _, 1 -> _`) without re-walking the
/// pattern's enum / struct / binding shapes.
pub(super) fn collect_literal_reprs(pattern: &Pattern, out: &mut Vec<String>) {
    match pattern {
        Pattern::Literal { value, .. } => out.push(literal_repr(value)),
        Pattern::Or { patterns, .. } => {
            for alt in patterns {
                collect_literal_reprs(alt, out);
            }
        }
        _ => {}
    }
}

/// Canonical surface-string form of a literal pattern's value.
/// Stable enough to use as a dedupe key for cross-arm literal-arm
/// reachability checks. Strings are wrapped in quotes so an `Int`
/// literal "true" never collides with a `Bool` literal `true`
/// (subjects in the same `match` always have the same type, so the
/// collision is theoretical, but the wrapping costs nothing).
pub(super) fn literal_repr(value: &Literal) -> String {
    match value {
        Literal::Bool(b) => b.to_string(),
        Literal::Float(s) => s.clone(),
        Literal::Int(s) => s.clone(),
        Literal::String(s) => format!("\"{s}\""),
        Literal::Unit => "()".to_string(),
    }
}

/// Diagnose when a `Pattern::Literal`'s value doesn't agree with
/// the subject type. No coercion — strict equality, matching the
/// rest of alpha's literal-vs-subject contract.
fn check_literal_matches_subject(
    value: &Literal,
    subject_ty: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !subject_ty.is_resolved() {
        return;
    }
    let lit_ty = literal_type(value, registry);
    if &lit_ty == subject_ty {
        return;
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "match literal pattern of type `{}` does not match subject type `{}`",
            display_resolution(&lit_ty, registry),
            display_resolution(subject_ty, registry),
        ),
        span,
    ));
}

/// True when `subject_ty` resolves to a primitive admitted as a
/// literal-comparable subject (`Bool` / `Int` / `Float` / `String`).
/// Patterns made entirely of catch-alls bypass this check at the
/// `resolve_match` level — any subject type is fine when the only
/// patterns are wildcards / bindings.
pub(super) fn is_match_subject_primitive(
    subject_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    is_primitive(subject_ty, registry, "Bool")
        || is_primitive(subject_ty, registry, "Float")
        || is_primitive(subject_ty, registry, "Int")
        || is_primitive(subject_ty, registry, "String")
}

/// Lookup the [`EnumDefinition`] whose `Global(id)` head matches
/// `subject_ty`. Returns `None` for non-enum / unresolved subjects.
/// Used by [`super::match_expr::resolve_match`] to drive the
/// structural exhaustiveness check.
pub(super) fn match_subject_enum<'a>(
    subject_ty: &ResolvedType,
    registry: &'a GlobalRegistry,
) -> Option<&'a EnumDefinition> {
    let Resolution::Global(id) = subject_ty.resolution else {
        return None;
    };
    let entry = registry.get(id)?;
    let GlobalKind::Enum(definition) = &entry.kind else {
        return None;
    };
    definition.as_ref()
}
