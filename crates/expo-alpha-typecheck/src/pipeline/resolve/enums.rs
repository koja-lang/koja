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

use super::coercion::{Compatible, check_compatible, coercion_span};
use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::structs::{lookup_type, validate_named_fields};
use super::types::{display_resolution, verify_bounds};

pub(super) fn resolve_enum_construction(
    type_path: &[String],
    variant: &str,
    data: &mut EnumConstructionData,
    expected: Option<&ResolvedType>,
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
        validate_variant_payload(&enum_label, variant_def, data, span, resolver, diagnostics);
        return ResolvedType::leaf(Resolution::Global(enum_id));
    }

    let expected_type_args = expected_type_args_for(expected, enum_id, type_params.len());

    if matches!(variant_def.data, ResolvedVariantData::Unit) {
        if let Some(args) = expected_type_args {
            return ResolvedType::Named {
                resolution: Resolution::Global(enum_id),
                type_args: args,
            };
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck cannot infer type parameters of `{enum_label}` from \
                 unit variant `{variant}` (no payload to constrain `{}`)",
                type_params.join("`, `"),
            ),
            span,
        ));
        return ResolvedType::Named {
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
        expected_type_args.as_deref(),
        span,
        resolver.registry,
        diagnostics,
    );
    let substituted = substitute_variant(variant_def, &subst, enum_id);
    validate_variant_payload(&enum_label, &substituted, data, span, resolver, diagnostics);
    let type_args = subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    ResolvedType::Named {
        resolution: Resolution::Global(enum_id),
        type_args,
    }
}

/// Pull a same-head expected type's `type_args` for use as an
/// inference fallback. Returns `Some(args)` only when `expected` is a
/// fully-resolved [`ResolvedType::Named`] pointing at `enum_id` with
/// the right arity; any mismatch (different head, partial-resolved
/// args, missing hint) returns `None` so the caller falls back to
/// payload-only inference. Bidirectional inference is best-effort —
/// when expected can't help, we keep the original diagnostic shape.
fn expected_type_args_for(
    expected: Option<&ResolvedType>,
    enum_id: GlobalRegistryId,
    arity: usize,
) -> Option<Vec<ResolvedType>> {
    let ResolvedType::Named {
        resolution: Resolution::Global(expected_id),
        type_args,
    } = expected?
    else {
        return None;
    };
    if *expected_id != enum_id || type_args.len() != arity {
        return None;
    }
    if !type_args.iter().all(|ty| ty.is_resolved()) {
        return None;
    }
    Some(type_args.clone())
}

/// Infer concrete `type_args` for a generic enum construction by
/// unifying each declared payload element's template against the
/// resolved type of the supplied value. Mirrors the struct path:
/// emits one diagnostic per [`Conflict`] and one per Phantom param.
/// Shape-mismatched constructions skip inference and let
/// [`validate_variant_payload`] surface the shape diagnostic.
///
/// `expected_type_args` is the bidirectional fallback — slots that
/// payload-driven inference can't pin (a `Result.Err(e)` whose `T`
/// only the surrounding context knows) get filled from the
/// surrounding expected type before the "cannot infer" diagnostic
/// fires.
fn infer_enum_type_args(
    callee: Callee<'_>,
    variant: &ResolvedEnumVariant,
    data: &EnumConstructionData,
    expected_type_args: Option<&[ResolvedType]>,
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
    if let Some(expected) = expected_type_args {
        for (slot, expected_ty) in subst.iter_mut().zip(expected.iter()) {
            if slot.is_none() {
                *slot = Some(expected_ty.clone());
            }
        }
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
    resolver: &mut Resolver<'_>,
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
                resolver,
                diagnostics,
            );
        }
        (ResolvedVariantData::Tuple(element_types), EnumConstructionData::Tuple(exprs)) => {
            validate_tuple_payload(
                &variant_label,
                element_types,
                exprs,
                span,
                resolver,
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
    resolver: &mut Resolver<'_>,
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
        match check_compatible(expr, actual, declared, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                resolver.coercions.insert(coercion_span(expr), width);
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument {} of `{variant_label}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        index + 1,
                        display_resolution(declared, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    expr.span,
                ));
            }
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument {} of `{variant_label}` expects `{}`, got `{}`",
                        index + 1,
                        display_resolution(declared, resolver.registry),
                        display_resolution(actual, resolver.registry),
                    ),
                    expr.span,
                ));
            }
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
