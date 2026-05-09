//! Plain-struct destructure pattern (`Type{ field: x, ... }`) and
//! the shared field-pattern walker re-used by struct-shaped enum
//! variants ([`super::enums::resolve_enum_struct_pattern`]).
//!
//! Plain destructure counts as an unconditional catch-all
//! ([`PatternCoverage::CatchAll`]) — it admits every value of the
//! struct subject — and emits its field bindings on the success
//! edge; lowering picks them up via
//! [`super::super::super::lower::patterns`].

use expo_ast::ast::{Diagnostic, FieldPattern, Pattern};
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::labels::{pattern_kind_label, pattern_span};
use expo_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::structs::lookup_type;
use super::super::types::display_resolution;
use super::{PatternCoverage, resolve_pattern};
use crate::pipeline::unify::substitute_resolved_type;
use crate::registry::{GlobalKind, ResolvedStructField};

pub(super) fn resolve_struct_pattern(
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
/// roster. Mirrors the enum-struct metadata helper in
/// [`super::enums`] for the non-variant case and substitutes
/// generic type-args via `subject_ty.type_args`, so
/// `Bag<Int>{ item: x }` views `item` as `Int`.
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
    let subject_args: Option<&[ResolvedType]> = match subject_ty {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } if *id == struct_id => Some(type_args.as_slice()),
        _ => None,
    };
    if subject_ty.is_resolved() && subject_args.is_none() {
        diagnostics.push(Diagnostic::error(
            format!(
                "match arm pattern targets `{}`, but the subject has type `{}`",
                entry.identifier,
                display_resolution(subject_ty, resolver.registry),
            ),
            span,
        ));
    }
    let subst: Vec<Option<ResolvedType>> = match subject_args {
        Some(args) => args.iter().cloned().map(Some).collect(),
        None => Vec::new(),
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
/// [`resolve_struct_pattern`] and
/// [`super::enums::resolve_enum_struct_pattern`].
pub(super) fn walk_field_patterns(
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
pub(super) fn resolve_field_patterns_unbound(
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

fn is_admitted_field_element(pat: &Pattern) -> bool {
    matches!(pat, Pattern::Binding { .. } | Pattern::Wildcard { .. })
}
