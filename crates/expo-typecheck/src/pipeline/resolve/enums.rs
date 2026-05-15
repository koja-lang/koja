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
use expo_ast::coercion::{Coercion, LiteralCoercion};
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex};
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, Substitution, substitute};
use crate::registry::{
    GlobalKind, GlobalRegistry, ResolvedEnumVariant, ResolvedStructField, ResolvedVariantData,
};

use super::coercion::{Compatible, check_compatible, coercion_annotation_mut, coercion_target_mut};
use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::inference::{PhantomContext, fill_from_expected, finalize_inference, unify_pairs};
use super::structs::validate_named_fields;
use super::types::{display_resolution, lookup_type};

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

    let Some((enum_id, enum_entry)) = lookup_type(type_path, resolver.resolution_scope()) else {
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

    let callee = Callee {
        id: enum_id,
        label: &enum_label,
        type_params: &type_params,
    };
    let subst = infer_enum_type_args(
        callee,
        variant_def,
        data,
        expected,
        span,
        resolver.registry,
        diagnostics,
    );
    let substituted = substitute_variant(variant_def, &subst);
    validate_variant_payload(&enum_label, &substituted, data, span, resolver, diagnostics);
    ResolvedType::Named {
        resolution: Resolution::Global(enum_id),
        type_args: subst.args(enum_id),
    }
}

/// Infer concrete `type_args` for a generic enum construction by
/// unifying each declared payload element's template against the
/// resolved type of the supplied value. Unit variants enter with no
/// payload pairs and rely entirely on bidirectional fallback —
/// emits one diagnostic per [`Conflict`] and one per phantom param.
/// Shape-mismatched constructions skip inference and let
/// [`validate_variant_payload`] surface the shape diagnostic.
///
/// `expected` is the bidirectional fallback — slots that payload-
/// driven inference can't pin (a `Result.Err(e)` whose `T` only
/// the surrounding context knows, or a unit `Maybe.None`) get
/// filled from the surrounding expected type before the "cannot
/// infer" diagnostic fires.
fn infer_enum_type_args(
    callee: Callee<'_>,
    variant: &ResolvedEnumVariant,
    data: &EnumConstructionData,
    expected: Option<&ResolvedType>,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Substitution {
    let mut subst = Substitution::single(callee.id, callee.type_params.len());
    let on_conflict = &mut |conflict: Conflict, payload_span: Span| {
        emit_conflict(
            &callee,
            &variant.name,
            conflict,
            payload_span,
            registry,
            diagnostics,
        );
    };
    match (&variant.data, data) {
        (ResolvedVariantData::Tuple(declared), EnumConstructionData::Tuple(exprs)) => {
            let pairs = declared
                .iter()
                .zip(exprs.iter())
                .map(|(declared_ty, expr)| (declared_ty, &expr.resolution, expr.span));
            unify_pairs(pairs, &mut subst, registry, on_conflict);
        }
        (ResolvedVariantData::Struct(declared), EnumConstructionData::Struct(inits)) => {
            let pairs = inits.iter().filter_map(|init| {
                let declared_field = declared.iter().find(|f| f.name == init.name)?;
                Some((&declared_field.ty, &init.value.resolution, init.span))
            });
            unify_pairs(pairs, &mut subst, registry, on_conflict);
        }
        _ => {}
    }
    if let Some(hint) = expected {
        let template = canonical_enum_template(callee.id, callee.type_params.len());
        fill_from_expected(&template, hint, &mut subst, registry);
    }
    let context = match variant.data {
        ResolvedVariantData::Unit => PhantomContext::UnitVariant(&variant.name),
        _ => PhantomContext::Payload(&variant.name),
    };
    finalize_inference(&[callee], &subst, &context, span, registry, diagnostics);
    subst
}

/// Build the enum's canonical self-referential template
/// `Named { Global(enum_id), [TypeParam(enum_id, 0..N)] }`. Used as the
/// LHS of a [`fill_from_expected`] hint walk so an expected `Maybe<Int>`
/// can populate `T → Int` without bespoke per-slot fill logic.
fn canonical_enum_template(enum_id: GlobalRegistryId, arity: usize) -> ResolvedType {
    ResolvedType::Named {
        resolution: Resolution::Global(enum_id),
        type_args: (0..arity)
            .map(|index| ResolvedType::Named {
                resolution: Resolution::TypeParam {
                    owner: enum_id,
                    index: TypeParamIndex::new(index as u32),
                },
                type_args: Vec::new(),
            })
            .collect(),
    }
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
fn substitute_variant(variant: &ResolvedEnumVariant, subst: &Substitution) -> ResolvedEnumVariant {
    let data = match &variant.data {
        ResolvedVariantData::Unit => ResolvedVariantData::Unit,
        ResolvedVariantData::Tuple(types) => {
            ResolvedVariantData::Tuple(types.iter().map(|ty| substitute(ty, subst)).collect())
        }
        ResolvedVariantData::Struct(fields) => ResolvedVariantData::Struct(
            fields
                .iter()
                .map(|field| ResolvedStructField {
                    name: field.name.clone(),
                    ty: substitute(&field.ty, subst),
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
    data: &mut EnumConstructionData,
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
    exprs: &mut [Expr],
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
    for (index, (declared, expr)) in element_types.iter().zip(exprs.iter_mut()).enumerate() {
        let actual = expr.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible(expr, &actual, declared, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                *coercion_target_mut(expr) = Some(LiteralCoercion::NumericLiteralWidth(width));
            }
            Compatible::UnionWiden { target } => {
                *coercion_annotation_mut(expr) = Some(Coercion::UnionWiden(target));
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
                        display_resolution(&actual, resolver.registry),
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
