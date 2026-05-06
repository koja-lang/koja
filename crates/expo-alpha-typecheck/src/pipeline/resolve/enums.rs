//! Enum-construction resolution.
//!
//! Mirrors [`super::structs::resolve_struct_construction`] but covers
//! all three variant payload shapes. Validates that the type path
//! resolves to a registered enum, the named variant exists, and the
//! supplied data shape matches the variant's declared payload
//! (Unit/Tuple/Struct). Per-shape validation:
//!
//! - **Unit**: data must be empty (`Color.Red`).
//! - **Tuple**: arity matches and each positional expr's resolved
//!   type matches the declared element type.
//! - **Struct**: delegates to [`super::structs::validate_named_fields`]
//!   for the named-field walk so the diagnostic surface is identical
//!   between struct construction and struct-variant construction.
//!
//! The expression's `ResolvedType` is always the enum's leaf type
//! regardless of per-init mismatches so the surrounding tree stays
//! stable.

use expo_ast::ast::{Diagnostic, EnumConstructionData, Expr};
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{GlobalKind, GlobalRegistry, ResolvedEnumVariant, ResolvedVariantData};

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::structs::{lookup_type, validate_named_fields};
use super::types::display_resolution;

pub(super) fn resolve_enum_construction(
    type_path: &[String],
    variant: &str,
    data: &mut EnumConstructionData,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Resolve every payload sub-expression up front so seal walks a
    // populated tree even if the enum or variant doesn't resolve.
    resolve_construction_data(data, resolver, diagnostics);

    let Some((enum_id, enum_entry)) = lookup_type(type_path, resolver.package, resolver.registry)
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the enum type `{}`",
                type_path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };

    let GlobalKind::Enum(definition) = &enum_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot construct variant `{variant}` of `{}`: it is a {}, not an enum",
                enum_entry.identifier,
                enum_entry.kind.label(),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    let Some(definition) = definition else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: enum `{}` has no lifted definition",
                enum_entry.identifier,
            ),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(enum_id));
    };

    let enum_label = enum_entry.identifier.to_string();
    let Some((_, variant_def)) = definition.lookup_variant(variant) else {
        diagnostics.push(Diagnostic::error(
            format!("`{enum_label}` has no variant `{variant}`"),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(enum_id));
    };

    validate_variant_payload(
        &enum_label,
        variant_def,
        data,
        span,
        resolver.registry,
        diagnostics,
    );

    ResolvedType::leaf(Resolution::Global(enum_id))
}

fn resolve_construction_data(
    data: &mut EnumConstructionData,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match data {
        EnumConstructionData::Struct(fields) => {
            for field in fields.iter_mut() {
                resolve_expr(&mut field.value, resolver, diagnostics);
            }
        }
        EnumConstructionData::Tuple(exprs) => {
            for expr in exprs.iter_mut() {
                resolve_expr(expr, resolver, diagnostics);
            }
        }
        EnumConstructionData::Unit => {}
    }
}

/// Per-shape validation. Each arm picks the right validator based on
/// the variant's declared `data` shape; mismatched shapes (e.g.
/// `Color.Red(42)` for a unit variant) produce one shape-mismatch
/// diagnostic and let the inner exprs keep their already-resolved
/// types.
fn validate_variant_payload(
    enum_label: &str,
    variant: &ResolvedEnumVariant,
    data: &EnumConstructionData,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let variant_label = format!("{enum_label}.{}", variant.name);
    match (&variant.data, data) {
        (ResolvedVariantData::Struct(declared), EnumConstructionData::Struct(fields)) => {
            validate_named_fields(
                &variant_label,
                declared,
                fields,
                span,
                registry,
                diagnostics,
            );
        }
        (ResolvedVariantData::Tuple(element_types), EnumConstructionData::Tuple(exprs)) => {
            validate_tuple_payload(
                &variant_label,
                element_types,
                exprs,
                span,
                registry,
                diagnostics,
            );
        }
        (ResolvedVariantData::Unit, EnumConstructionData::Unit) => {}
        (declared, supplied) => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "variant `{variant_label}` is {}, not {}",
                    declared_shape_label(declared),
                    supplied_shape_label(supplied),
                ),
                span,
            ));
        }
    }
}

fn validate_tuple_payload(
    variant_label: &str,
    element_types: &[ResolvedType],
    exprs: &[Expr],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if exprs.len() != element_types.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "variant `{variant_label}` expects {} positional argument{}, got {}",
                element_types.len(),
                if element_types.len() == 1 { "" } else { "s" },
                exprs.len(),
            ),
            span,
        ));
        return;
    }
    for (index, (declared, expr)) in element_types.iter().zip(exprs.iter()).enumerate() {
        let actual = &expr.resolution;
        if !actual.is_resolved() {
            continue;
        }
        if actual != declared {
            diagnostics.push(Diagnostic::error(
                format!(
                    "argument {} of `{variant_label}` expects `{}`, got `{}`",
                    index + 1,
                    display_resolution(declared, registry),
                    display_resolution(actual, registry),
                ),
                expr.span,
            ));
        }
    }
}

fn declared_shape_label(data: &ResolvedVariantData) -> &'static str {
    match data {
        ResolvedVariantData::Struct(_) => "a struct variant (named fields)",
        ResolvedVariantData::Tuple(_) => "a tuple variant (positional fields)",
        ResolvedVariantData::Unit => "a unit variant (no payload)",
    }
}

fn supplied_shape_label(data: &EnumConstructionData) -> &'static str {
    match data {
        EnumConstructionData::Struct(_) => "constructed with named fields",
        EnumConstructionData::Tuple(_) => "constructed with positional arguments",
        EnumConstructionData::Unit => "constructed with no payload",
    }
}
