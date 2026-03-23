//! Pattern matching validation and exhaustiveness checking.
//!
//! Validates match arm patterns against their subject types, binds pattern
//! variables into the type environment, and checks that match expressions
//! cover all enum variants.

use std::collections::HashMap;

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::check::check_literal_overflow;
use crate::context::{TypeContext, VariantData};
use crate::env::{VarInfo, VarState};
use crate::types::{
    GenericKind, Primitive, Type, build_substitution, resolve_type_expr, substitute,
};

fn pattern_is_catch_all(pat: &Pattern) -> bool {
    match pat {
        Pattern::Wildcard { .. } | Pattern::Binding { .. } => true,
        Pattern::Or { patterns, .. } => patterns.iter().any(pattern_is_catch_all),
        _ => false,
    }
}

/// Collects all leaf patterns from an arm, flattening OR patterns.
fn flatten_pattern(pat: &Pattern) -> Vec<&Pattern> {
    match pat {
        Pattern::Or { patterns, .. } => patterns.iter().flat_map(flatten_pattern).collect(),
        other => vec![other],
    }
}

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
    let has_catch_all = arms
        .iter()
        .any(|arm| arm.guard.is_none() && pattern_is_catch_all(&arm.pattern));
    if has_catch_all {
        return;
    }

    match subject_type {
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits) => {
            let has_binary_pattern = arms
                .iter()
                .any(|arm| matches!(arm.pattern, Pattern::Binary { .. }));
            if has_binary_pattern {
                ctx.error_with_hint(
                    "non-exhaustive match on binary data: missing catch-all".to_string(),
                    "binary patterns cannot cover all inputs -- add a `_ ->` catch-all arm".into(),
                    span,
                );
            }
        }
        Type::Enum(enum_name) => {
            let Some(type_info) = ctx.types.get(enum_name) else {
                return;
            };
            let Some(variants) = type_info.variants() else {
                return;
            };

            let matched: Vec<&str> = arms
                .iter()
                .filter(|arm| arm.guard.is_none())
                .flat_map(|arm| flatten_pattern(&arm.pattern))
                .filter_map(|pat| match pat {
                    Pattern::EnumUnit { variant, .. }
                    | Pattern::EnumTuple { variant, .. }
                    | Pattern::EnumStruct { variant, .. } => Some(variant.as_str()),
                    Pattern::Constructor { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();

            let missing: Vec<&str> = variants
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

            let struct_names = ctx.struct_names();
            let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
            let enum_name_keys = ctx.enum_names();
            let enum_refs: Vec<&str> = enum_name_keys.iter().map(|s| s.as_str()).collect();

            let matched: Vec<String> = arms
                .iter()
                .filter(|arm| arm.guard.is_none())
                .flat_map(|arm| flatten_pattern(&arm.pattern))
                .filter_map(|pat| match pat {
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
    let type_info = ctx.types.get(enum_name)?;
    let variants = type_info.variants()?;
    let vi = variants.iter().find(|v| v.name == *variant)?;
    let data = vi.data.clone();

    if let Type::GenericInstance {
        type_args,
        kind: GenericKind::Enum,
        ..
    } = effective_ty
        && !type_info.type_params.is_empty()
    {
        let subst = build_substitution(&type_info.type_params, type_args);
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
                    if ctx.is_enum(&enum_name) {
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
                    if ctx.is_enum(&enum_name) {
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
            if let Some(type_info) = ctx.types.get(&enum_name).filter(|ti| ti.is_enum()) {
                let variants = type_info.variants().unwrap();
                if let Some(vi) = variants.iter().find(|v| v.name == *variant) {
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
                Literal::String(_) => Type::Primitive(Primitive::String),
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
            let struct_names = ctx.struct_names();
            let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
            let enum_names = ctx.enum_names();
            let enum_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();
            let resolved = resolve_type_expr(type_expr, &struct_refs, &enum_refs);

            if let Type::Union(members) = subject_type {
                if !members.contains(&resolved) {
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

        Pattern::Binary { segments, span } => {
            check_binary_pattern(segments, subject_type, *span, ctx, env);
        }

        Pattern::Or { patterns, span } => {
            for sub in patterns {
                let bindings = collect_pattern_bindings(sub);
                if !bindings.is_empty() {
                    ctx.error(
                        "variable bindings are not allowed inside OR patterns".to_string(),
                        bindings[0].1,
                    );
                    return;
                }
                check_pattern(sub, subject_type_raw, ctx, env);
            }
            let _ = span;
        }

        Pattern::Wildcard { .. } => {}
    }
}

/// Validates a binary pattern's segments: assigns binding types, checks greedy
/// rest rules, and validates modifier usage.
fn check_binary_pattern(
    segments: &[BinarySegment],
    subject_type: &Type,
    span: Span,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, VarInfo>,
) {
    if subject_type.is_known()
        && !matches!(
            subject_type,
            Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits)
        )
    {
        ctx.error(
            format!(
                "binary pattern requires `Binary` or `Bits` subject, found `{}`",
                subject_type.display()
            ),
            span,
        );
    }

    let struct_names = ctx.struct_names();
    let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_name_keys = ctx.enum_names();
    let enum_refs: Vec<&str> = enum_name_keys.iter().map(|s| s.as_str()).collect();

    let mut total_fixed_bits: u64 = 0;
    let mut has_greedy = false;

    for (i, seg) in segments.iter().enumerate() {
        let is_last = i == segments.len() - 1;

        let is_binding = matches!(seg.value.as_ref(), Expr::Ident { name, .. } if name != "_");
        let is_discard = matches!(seg.value.as_ref(), Expr::Ident { name, .. } if name == "_");
        let is_literal = matches!(
            seg.value.as_ref(),
            Expr::Literal { .. } | Expr::Unary { .. }
        );

        let is_greedy_rest = seg.type_ann.is_some() && seg.size.is_none() && {
            let ann_ty =
                resolve_type_expr(seg.type_ann.as_ref().unwrap(), &struct_refs, &enum_refs);
            matches!(
                ann_ty,
                Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits)
            )
        };

        if is_greedy_rest {
            if has_greedy {
                ctx.error(
                    "at most one greedy rest segment allowed per binary pattern".to_string(),
                    seg.span,
                );
            }
            if !is_last {
                ctx.error(
                    "greedy rest segment must be the last segment".to_string(),
                    seg.span,
                );
            }
            has_greedy = true;

            let ann_ty =
                resolve_type_expr(seg.type_ann.as_ref().unwrap(), &struct_refs, &enum_refs);
            if matches!(ann_ty, Type::Primitive(Primitive::Binary))
                && !total_fixed_bits.is_multiple_of(8)
            {
                ctx.error(
                    format!(
                        "`: Binary` rest requires byte-aligned prefix, but fixed prefix is {} bits",
                        total_fixed_bits
                    ),
                    seg.span,
                );
            }

            if is_binding && let Expr::Ident { name, .. } = seg.value.as_ref() {
                env.insert(
                    name.clone(),
                    VarInfo {
                        ty: ann_ty,
                        state: VarState::Live,
                    },
                );
            }
            continue;
        }

        let seg_bits: Option<u64> = if let Some(size_expr) = &seg.size {
            if let Expr::Literal {
                value: Literal::Int(n),
                ..
            } = size_expr.as_ref()
            {
                if let Ok(bits) = n.parse::<u64>() {
                    let actual = if seg.unit == BinaryUnit::Byte {
                        bits * 8
                    } else {
                        bits
                    };
                    Some(actual)
                } else {
                    ctx.error(
                        "segment size must be a non-negative integer literal".to_string(),
                        seg.span,
                    );
                    None
                }
            } else {
                ctx.error(
                    "segment size in patterns must be a literal integer".to_string(),
                    seg.span,
                );
                None
            }
        } else if let Some(type_ann) = &seg.type_ann {
            let ann_ty = resolve_type_expr(type_ann, &struct_refs, &enum_refs);
            match &ann_ty {
                Type::Primitive(p) => {
                    if let Some(w) = p.bit_width() {
                        Some(w)
                    } else {
                        ctx.error(
                            format!(
                                "type `{}` has no fixed bit width in binary pattern",
                                p.display()
                            ),
                            seg.span,
                        );
                        None
                    }
                }
                _ => {
                    ctx.error(
                        format!(
                            "segment type must be a primitive type, found `{}`",
                            ann_ty.display()
                        ),
                        seg.span,
                    );
                    None
                }
            }
        } else {
            Some(8)
        };

        if let Some(bits) = seg_bits {
            total_fixed_bits += bits;
        }

        if is_binding {
            if let Expr::Ident { name, .. } = seg.value.as_ref() {
                let binding_ty = if let Some(type_ann) = &seg.type_ann {
                    resolve_type_expr(type_ann, &struct_refs, &enum_refs)
                } else if seg.size.is_some() && seg.unit == BinaryUnit::Byte {
                    Type::Primitive(Primitive::Binary)
                } else {
                    Type::Primitive(Primitive::I64)
                };
                env.insert(
                    name.clone(),
                    VarInfo {
                        ty: binding_ty,
                        state: VarState::Live,
                    },
                );
            }
        } else if is_literal {
            if let Some(bits) = seg_bits {
                check_literal_overflow(&seg.value, bits, seg.signedness, seg.span, ctx);
            }
        } else if is_discard {
            // skip
        }

        if seg.signedness.is_some() && seg.size.is_none() && seg.type_ann.is_none() {
            ctx.error(
                "signedness modifier requires a size specifier (::N)".to_string(),
                seg.span,
            );
        }
        if seg.endianness.is_some() && seg.size.is_none() && seg.type_ann.is_none() {
            ctx.error(
                "endianness modifier requires a size specifier (::N)".to_string(),
                seg.span,
            );
        }
    }
}

/// Collects all variable bindings from a pattern (name and span).
pub(crate) fn collect_pattern_bindings(pat: &Pattern) -> Vec<(String, Span)> {
    let mut bindings = Vec::new();
    collect_bindings_inner(pat, &mut bindings);
    bindings
}

/// Recursively walks a pattern, appending every binding name and its span to `out`.
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
        Pattern::Or { .. } => {}
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::EnumUnit { .. } => {}
    }
}

/// Extracts binding identifiers from binary segment value expressions.
fn collect_bindings_inner_expr(expr: &Expr, out: &mut Vec<(String, Span)>) {
    if let Expr::Ident { name, span } = expr
        && name != "_"
    {
        out.push((name.clone(), *span));
    }
}
