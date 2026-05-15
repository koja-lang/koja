//! Plain-struct destructure pattern (`Type{ field: x, ... }`) and
//! the shared field-pattern walker re-used by struct-shaped enum
//! variants ([`super::enums::resolve_enum_struct_pattern`]).
//!
//! Field positions accept any pattern shape — wildcards, bindings,
//! literals, nested structs, nested enums, or-alternatives. Coverage
//! is `PatternCoverage::CatchAll` only when every *listed* field's
//! own coverage is catch-all (omitted fields are implicit
//! wildcards); otherwise `PatternCoverage::Other`. IR lowering picks
//! up the field bindings and any chained literal checks via
//! [`super::super::super::lower::patterns`].

use expo_ast::ast::{Diagnostic, FieldPattern};
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::types::{display_resolution, lookup_type};
use super::{PatternCoverage, resolve_pattern};
use crate::pipeline::unify::{Substitution, substitute};
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
    )
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
    let Some((struct_id, entry)) = lookup_type(type_path, resolver.resolution_scope()) else {
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
    let subst = match subject_args {
        Some(args) => Substitution::from_args(struct_id, args),
        None => Substitution::empty(),
    };
    let declared = definition
        .fields
        .iter()
        .map(|field| ResolvedStructField {
            name: field.name.clone(),
            ty: substitute(&field.ty, &subst),
        })
        .collect();
    Some(StructPatternMetadata {
        declared,
        label: entry.identifier.to_string(),
    })
}

/// Walk a `FieldPattern` list against a substituted declared
/// roster: lookup by name, recurse into the sub-pattern with the
/// field's declared type. Diagnoses unknown fields and duplicate
/// field-name patterns. Returns the merged coverage across the
/// listed fields: catch-all only when every listed field's own
/// coverage is catch-all (omitted fields are implicit wildcards).
/// Used by both [`resolve_struct_pattern`] and
/// [`super::enums::resolve_enum_struct_pattern`].
pub(super) fn walk_field_patterns(
    owner_label: &str,
    fields: &mut [FieldPattern],
    declared: &[ResolvedStructField],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let mut seen: Vec<bool> = vec![false; declared.len()];
    let mut all_catch_all = true;
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
        let field_coverage = resolve_pattern(
            &mut field.pattern,
            &declared_field.ty,
            resolver,
            diagnostics,
        );
        if !matches!(field_coverage, PatternCoverage::CatchAll) {
            all_catch_all = false;
        }
    }
    if all_catch_all {
        PatternCoverage::CatchAll
    } else {
        PatternCoverage::Other
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
