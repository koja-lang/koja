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
use crate::types::{
    GenericKind, Primitive, Type, build_substitution, resolve_type_expr, substitute,
};

/// Checks whether a match expression covers all variants of an enum subject,
/// emitting a diagnostic if any variants are missing and no catch-all exists.
pub(crate) fn check_match_exhaustiveness(
    subject_type_raw: &Type,
    arms: &[MatchArm],
    span: Span,
    ctx: &mut TypeContext,
) {
    let subject_type = match subject_type_raw {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
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

    match subject_type {
        Type::Enum(enum_name) => {
            let Some(enum_info) = ctx.enums.get(enum_name) else {
                return;
            };

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
        Type::Union(members) => {
            let member_names: Vec<String> = members.iter().map(|m| m.display()).collect();

            let struct_names: Vec<String> = ctx.structs.keys().cloned().collect();
            let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
            let enum_name_keys: Vec<String> = ctx.enums.keys().cloned().collect();
            let enum_refs: Vec<&str> = enum_name_keys.iter().map(|s| s.as_str()).collect();

            let matched: Vec<String> = arms
                .iter()
                .filter(|arm| arm.guard.is_none())
                .filter_map(|arm| match &arm.pattern {
                    Pattern::EnumUnit { type_path, .. } => Some(type_path.join(".")),
                    Pattern::EnumTuple { type_path, .. } => Some(type_path.join(".")),
                    Pattern::EnumStruct { type_path, .. } => Some(type_path.join(".")),
                    Pattern::Constructor { name, .. } => Some(name.clone()),
                    Pattern::TypedBinding { type_expr, .. } => {
                        let resolved = resolve_type_expr(type_expr, &struct_refs, &enum_refs);
                        Some(resolved.display())
                    }
                    Pattern::Binding { .. } => None,
                    _ => None,
                })
                .collect();

            let missing: Vec<&str> = member_names
                .iter()
                .filter(|name| !matched.contains(name))
                .map(|n| n.as_str())
                .collect();

            if !missing.is_empty() {
                let missing_list = missing.join(", ");
                ctx.error_with_hint(
                    format!(
                        "non-exhaustive match on union type: missing `{}`",
                        missing_list
                    ),
                    format!("add a `_ ->` catch-all or handle: {}", missing_list),
                    span,
                );
            }
        }
        _ => {}
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
    let effective_ty = match subject_type {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    let enum_info = ctx.enums.get(enum_name)?;
    let vi = enum_info.variants.iter().find(|v| v.name == *variant)?;
    let data = vi.data.clone();

    if let Type::GenericInstance {
        type_args,
        kind: GenericKind::Enum,
        ..
    } = effective_ty
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
    subject_type_raw: &Type,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, VarInfo>,
) {
    let subject_type = match subject_type_raw {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
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
                Literal::Int(_) => Type::Primitive(Primitive::I64),
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

        Pattern::TypedBinding {
            name,
            type_expr,
            span,
        } => {
            let struct_names: Vec<String> = ctx.structs.keys().cloned().collect();
            let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
            let enum_names: Vec<String> = ctx.enums.keys().cloned().collect();
            let enum_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();
            let resolved = resolve_type_expr(type_expr, &struct_refs, &enum_refs);

            if let Type::Union(members) = subject_type {
                if !members.iter().any(|m| m.display() == resolved.display()) {
                    ctx.error(
                        format!(
                            "type `{}` is not a member of union `{}`",
                            resolved.display(),
                            subject_type.display()
                        ),
                        *span,
                    );
                }
            } else if subject_type.is_known() && resolved.display() != subject_type.display() {
                ctx.error(
                    format!(
                        "typed binding pattern on non-union type `{}`",
                        subject_type.display()
                    ),
                    *span,
                );
            }

            env.insert(
                name.clone(),
                VarInfo {
                    ty: resolved,
                    state: VarState::Live,
                },
            );
        }

        Pattern::Binary { span, .. } => {
            ctx.error(
                "binary patterns are not yet type-checked".to_string(),
                *span,
            );
        }

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
        Pattern::TypedBinding { name, span, .. } => {
            out.push((name.clone(), *span));
        }
        Pattern::Binary { segments, .. } => {
            for seg in segments {
                collect_bindings_inner_expr(&seg.value, out);
            }
        }
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::EnumUnit { .. } => {}
    }
}

fn collect_bindings_inner_expr(expr: &Expr, out: &mut Vec<(String, Span)>) {
    if let Expr::Ident { name, span } = expr {
        if name != "_" {
            out.push((name.clone(), *span));
        }
    }
}
