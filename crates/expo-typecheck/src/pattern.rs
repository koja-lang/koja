//! Pattern matching validation and exhaustiveness checking.
//!
//! Validates match arm patterns against their subject types, binds pattern
//! variables into the type environment, and checks that match expressions
//! cover all enum variants.

use std::collections::HashMap;

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::{TypeContext, VariantData};
use crate::env::{VarInfo, VarState};
use crate::types::{GenericKind, Primitive, Type, build_substitution, substitute};

/// Checks whether a match expression covers all variants of an enum subject,
/// emitting a diagnostic if any variants are missing and no catch-all exists.
pub(crate) fn check_match_exhaustiveness(
    subject_type: &Type,
    arms: &[MatchArm],
    span: Span,
    ctx: &mut TypeContext,
) {
    let Type::Enum(enum_name) = subject_type else {
        return;
    };
    let Some(enum_info) = ctx.enums.get(enum_name) else {
        return;
    };

    let has_catch_all = arms.iter().any(|arm| {
        matches!(
            arm.pattern,
            Pattern::Wildcard { .. } | Pattern::Binding { .. }
        ) && arm.guard.is_none()
    });
    if has_catch_all {
        return;
    }

    let matched: Vec<&str> = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .filter_map(|arm| match &arm.pattern {
            Pattern::EnumUnit { variant, .. }
            | Pattern::EnumTuple { variant, .. }
            | Pattern::EnumStruct { variant, .. } => Some(variant.as_str()),
            Pattern::Constructor { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    let missing: Vec<&str> = enum_info
        .variants
        .iter()
        .filter(|v| !matched.contains(&v.name.as_str()))
        .map(|v| v.name.as_str())
        .collect();

    if !missing.is_empty() {
        let missing_list = missing.join(", ");
        ctx.error_with_hint(
            format!(
                "non-exhaustive match: missing variant(s) `{}`",
                missing_list
            ),
            format!("add a `_ ->` catch-all or handle: {}", missing_list),
            span,
        );
    }
}

/// Resolves the variant data for a pattern, applying type substitution for
/// generic enums when the subject type is a `GenericInstance`.
pub(crate) fn resolve_variant_data(
    enum_name: &str,
    variant: &str,
    subject_type: &Type,
    ctx: &TypeContext,
) -> Option<VariantData> {
    let enum_info = ctx.enums.get(enum_name)?;
    let vi = enum_info.variants.iter().find(|v| v.name == *variant)?;
    let data = vi.data.clone();

    if let Type::GenericInstance {
        type_args,
        kind: GenericKind::Enum,
        ..
    } = subject_type
        && !enum_info.type_params.is_empty()
    {
        let subst = build_substitution(&enum_info.type_params, type_args);
        return Some(substitute_variant_data(&data, &subst));
    }
    Some(data)
}

/// Substitutes type parameters in variant data using the given mapping.
fn substitute_variant_data(data: &VariantData, subst: &HashMap<String, Type>) -> VariantData {
    match data {
        VariantData::Unit => VariantData::Unit,
        VariantData::Tuple(types) => {
            VariantData::Tuple(types.iter().map(|t| substitute(t, subst)).collect())
        }
        VariantData::Struct(fields) => VariantData::Struct(
            fields
                .iter()
                .map(|(n, t)| (n.clone(), substitute(t, subst)))
                .collect(),
        ),
    }
}

/// Recursively validates a match pattern against the expected subject type,
/// binding pattern variables into the environment.
pub(crate) fn check_pattern(
    pat: &Pattern,
    subject_type: &Type,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, VarInfo>,
) {
    match pat {
        Pattern::Binding { name, .. } => {
            env.insert(
                name.clone(),
                VarInfo {
                    ty: subject_type.clone(),
                    state: VarState::Live,
                },
            );
        }

        Pattern::Constructor {
            name: _, elements, ..
        } => {
            for sub_pat in elements {
                check_pattern(sub_pat, &Type::Unknown, ctx, env);
            }
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            span,
        } => {
            let enum_name = type_path.join(".");
            let variant_data = resolve_variant_data(&enum_name, variant, subject_type, ctx);

            match variant_data {
                Some(VariantData::Struct(expected_fields)) => {
                    for fp in fields {
                        if let Some((_, field_ty)) =
                            expected_fields.iter().find(|(n, _)| *n == fp.name)
                        {
                            if let Some(sub_pat) = &fp.pattern {
                                check_pattern(sub_pat, field_ty, ctx, env);
                            } else {
                                env.insert(
                                    fp.name.clone(),
                                    VarInfo {
                                        ty: field_ty.clone(),
                                        state: VarState::Live,
                                    },
                                );
                            }
                        } else {
                            ctx.error(
                                format!(
                                    "variant `{}.{}` has no field `{}`",
                                    enum_name, variant, fp.name
                                ),
                                fp.span,
                            );
                        }
                    }
                }
                Some(VariantData::Unit) => {
                    ctx.error(
                        format!("variant `{}.{}` has no fields", enum_name, variant),
                        *span,
                    );
                }
                Some(VariantData::Tuple(_)) => {
                    ctx.error(
                        format!(
                            "variant `{}.{}` has positional fields, use ( ) pattern",
                            enum_name, variant
                        ),
                        *span,
                    );
                }
                None => {
                    if ctx.enums.contains_key(&enum_name) {
                        ctx.error(
                            format!("enum `{}` has no variant `{}`", enum_name, variant),
                            *span,
                        );
                    }
                }
            }
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            span,
        } => {
            let enum_name = type_path.join(".");
            let variant_data = resolve_variant_data(&enum_name, variant, subject_type, ctx);

            match variant_data {
                Some(VariantData::Tuple(expected_types)) => {
                    if elements.len() != expected_types.len() {
                        ctx.error(
                            format!(
                                "variant `{}.{}` has {} fields, pattern has {}",
                                enum_name,
                                variant,
                                expected_types.len(),
                                elements.len()
                            ),
                            *span,
                        );
                    } else {
                        for (sub_pat, expected_ty) in elements.iter().zip(expected_types.iter()) {
                            check_pattern(sub_pat, expected_ty, ctx, env);
                        }
                    }
                }
                Some(VariantData::Unit) => {
                    ctx.error(
                        format!("variant `{}.{}` has no fields", enum_name, variant),
                        *span,
                    );
                }
                Some(VariantData::Struct(_)) => {
                    ctx.error(
                        format!(
                            "variant `{}.{}` has named fields, use {{ }} pattern",
                            enum_name, variant
                        ),
                        *span,
                    );
                }
                None => {
                    if ctx.enums.contains_key(&enum_name) {
                        ctx.error(
                            format!("enum `{}` has no variant `{}`", enum_name, variant),
                            *span,
                        );
                    }
                }
            }
        }

        Pattern::EnumUnit {
            type_path,
            variant,
            span,
        } => {
            let enum_name = type_path.join(".");
            if let Some(enum_info) = ctx.enums.get(&enum_name) {
                if let Some(vi) = enum_info.variants.iter().find(|v| v.name == *variant) {
                    if !matches!(vi.data, VariantData::Unit) {
                        ctx.error(
                            format!("variant `{}.{}` requires arguments", enum_name, variant),
                            *span,
                        );
                    }
                } else {
                    ctx.error(
                        format!("enum `{}` has no variant `{}`", enum_name, variant),
                        *span,
                    );
                }
            }
        }

        Pattern::List { elements, .. } => {
            for sub_pat in elements {
                check_pattern(sub_pat, &Type::Unknown, ctx, env);
            }
        }

        Pattern::Literal { value, span } => {
            let lit_type = match value {
                Literal::Bool(_) => Type::Primitive(Primitive::Bool),
                Literal::Float(_) => Type::Primitive(Primitive::F64),
                Literal::Int(_) => Type::Primitive(Primitive::I32),
                Literal::Unit => Type::Unit,
            };
            if lit_type.is_known() && subject_type.is_known() && lit_type != *subject_type {
                ctx.error(
                    format!(
                        "pattern type mismatch: matching `{}` against `{}`",
                        lit_type.display(),
                        subject_type.display()
                    ),
                    *span,
                );
            }
        }

        Pattern::Tuple { elements, span } => match subject_type {
            Type::Tuple(expected_types) => {
                if elements.len() != expected_types.len() {
                    ctx.error(
                        format!(
                            "tuple pattern has {} elements, expected {}",
                            elements.len(),
                            expected_types.len()
                        ),
                        *span,
                    );
                } else {
                    for (sub_pat, expected_ty) in elements.iter().zip(expected_types.iter()) {
                        check_pattern(sub_pat, expected_ty, ctx, env);
                    }
                }
            }
            Type::Unknown | Type::Error => {
                for sub_pat in elements {
                    check_pattern(sub_pat, &Type::Unknown, ctx, env);
                }
            }
            _ => {
                ctx.error(
                    format!(
                        "tuple pattern on non-tuple type `{}`",
                        subject_type.display()
                    ),
                    *span,
                );
            }
        },

        Pattern::Wildcard { .. } => {}
    }
}

/// Collects all variable bindings from a pattern (name and span).
pub(crate) fn collect_pattern_bindings(pat: &Pattern) -> Vec<(String, Span)> {
    let mut bindings = Vec::new();
    collect_bindings_inner(pat, &mut bindings);
    bindings
}

fn collect_bindings_inner(pat: &Pattern, out: &mut Vec<(String, Span)>) {
    match pat {
        Pattern::Binding { name, span, .. } => {
            out.push((name.clone(), *span));
        }
        Pattern::EnumTuple { elements, .. }
        | Pattern::Tuple { elements, .. }
        | Pattern::Constructor { elements, .. }
        | Pattern::List { elements, .. } => {
            for sub in elements {
                collect_bindings_inner(sub, out);
            }
        }
        Pattern::EnumStruct { fields, .. } => {
            for f in fields {
                if let Some(sub) = &f.pattern {
                    collect_bindings_inner(sub, out);
                } else {
                    out.push((f.name.clone(), f.span));
                }
            }
        }
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::EnumUnit { .. } => {}
    }
}
