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
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{
    GlobalKind, GlobalRegistry, ResolvedEnumVariant, ResolvedStructField, ResolvedVariantData,
};

use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::structs::{lookup_type, validate_named_fields};
use super::types::{display_resolution, verify_bounds};

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
    let type_params = enum_entry.type_params.clone();
    let Some((_, variant_def)) = definition.lookup_variant(variant) else {
        diagnostics.push(Diagnostic::error(
            format!("`{enum_label}` has no variant `{variant}`"),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(enum_id));
    };

    if type_params.is_empty() {
        validate_variant_payload(
            &enum_label,
            variant_def,
            data,
            span,
            resolver.registry,
            diagnostics,
        );
        return ResolvedType::leaf(Resolution::Global(enum_id));
    }

    if matches!(variant_def.data, ResolvedVariantData::Unit) {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck cannot infer type parameters of `{enum_label}` from \
                 unit variant `{variant}` (no payload to constrain `{}`)",
                type_params.join("`, `"),
            ),
            span,
        ));
        return ResolvedType {
            resolution: Resolution::Global(enum_id),
            type_args: vec![ResolvedType::unresolved(); type_params.len()],
        };
    }

    let callee = Callee {
        id: enum_id,
        label: &enum_label,
        type_params: &type_params,
    };
    let subst = infer_enum_type_args(
        callee,
        variant_def,
        data,
        span,
        resolver.registry,
        diagnostics,
    );
    let substituted = substitute_variant(variant_def, &subst, enum_id);
    validate_variant_payload(
        &enum_label,
        &substituted,
        data,
        span,
        resolver.registry,
        diagnostics,
    );
    let type_args = subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    ResolvedType {
        resolution: Resolution::Global(enum_id),
        type_args,
    }
}

/// Infer concrete `type_args` for a generic enum construction by
/// unifying each declared payload element's template against the
/// resolved type of the supplied value. Mirrors the struct path:
/// emits one diagnostic per [`Conflict`] and one per Phantom param.
/// Shape-mismatched constructions skip inference and let
/// [`validate_variant_payload`] surface the shape diagnostic.
fn infer_enum_type_args(
    callee: Callee<'_>,
    variant: &ResolvedEnumVariant,
    data: &EnumConstructionData,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<ResolvedType>> {
    let mut subst: Vec<Option<ResolvedType>> = vec![None; callee.type_params.len()];
    match (&variant.data, data) {
        (ResolvedVariantData::Tuple(declared), EnumConstructionData::Tuple(exprs)) => {
            for (declared_ty, expr) in declared.iter().zip(exprs.iter()) {
                if !expr.resolution.is_resolved() {
                    continue;
                }
                if let Err(conflict) =
                    unify_resolved_type(declared_ty, &expr.resolution, callee.id, &mut subst)
                {
                    emit_conflict(
                        &callee,
                        &variant.name,
                        conflict,
                        expr.span,
                        registry,
                        diagnostics,
                    );
                }
            }
        }
        (ResolvedVariantData::Struct(declared), EnumConstructionData::Struct(inits)) => {
            for init in inits {
                let Some(declared_field) = declared.iter().find(|f| f.name == init.name) else {
                    continue;
                };
                if !init.value.resolution.is_resolved() {
                    continue;
                }
                if let Err(conflict) = unify_resolved_type(
                    &declared_field.ty,
                    &init.value.resolution,
                    callee.id,
                    &mut subst,
                ) {
                    emit_conflict(
                        &callee,
                        &variant.name,
                        conflict,
                        init.span,
                        registry,
                        diagnostics,
                    );
                }
            }
        }
        _ => {}
    }
    for (index, slot) in subst.iter().enumerate() {
        if slot.is_none() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck cannot infer type parameter `{}` of `{}` \
                     from the supplied `{}` payload",
                    callee.type_params[index], callee.label, variant.name,
                ),
                span,
            ));
        }
    }
    verify_bounds(callee, &subst, span, registry, diagnostics);
    subst
}

fn emit_conflict(
    callee: &Callee<'_>,
    variant_name: &str,
    conflict: Conflict,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "type parameter `{}` of `{}` cannot be both `{}` and `{}` in `{}`",
            callee.type_params[conflict.param_index],
            callee.label,
            display_resolution(&conflict.prev, registry),
            display_resolution(&conflict.actual, registry),
            variant_name,
        ),
        span,
    ));
}

/// Substitute a populated `subst` into every declared payload type
/// of `variant`. Used to produce a concrete view of the variant for
/// [`validate_variant_payload`] so user-facing diagnostics show the
/// inferred concrete types rather than `T`.
fn substitute_variant(
    variant: &ResolvedEnumVariant,
    subst: &[Option<ResolvedType>],
    owner: GlobalRegistryId,
) -> ResolvedEnumVariant {
    let data = match &variant.data {
        ResolvedVariantData::Unit => ResolvedVariantData::Unit,
        ResolvedVariantData::Tuple(types) => ResolvedVariantData::Tuple(
            types
                .iter()
                .map(|ty| substitute_resolved_type(ty, subst, owner))
                .collect(),
        ),
        ResolvedVariantData::Struct(fields) => ResolvedVariantData::Struct(
            fields
                .iter()
                .map(|field| ResolvedStructField {
                    name: field.name.clone(),
                    ty: substitute_resolved_type(&field.ty, subst, owner),
                })
                .collect(),
        ),
    };
    ResolvedEnumVariant {
        data,
        name: variant.name.clone(),
    }
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
