//! Lowering for `match` arms and the patterns within them.
//!
//! Walks the AST patterns alongside the subject's resolved type, picks the
//! right enum tag / struct field layout / tuple element decomposition,
//! and produces [`crate::resolved::patterns`] / [`crate::resolved::match_expr`]
//! values that the match-emission scaffolding can consume mechanically.

use expo_ast::ast::{Expr, ExprKind, FieldPattern, Literal, MatchArm, Pattern};
use expo_ast::identifier::{Package, TypeIdentifier};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, mangle_name, mangle_type, named, unwrap_indirect};

use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::{
    find_type_current, monomorphize_type, resolve_name_current, resolve_type_expr,
};
use crate::resolved::match_expr::{ResolvedMatch, ResolvedMatchType};
use crate::resolved::patterns::{ResolvedFieldPattern, ResolvedLiteral, ResolvedPattern};
use crate::util::parse_int_literal;

/// Picks the most specific Expo type available for the match subject.
/// Prefers the post-emit `expo_type` (codegen has full monomorphization
/// context, even inside generic impl bodies whose typechecked
/// `resolved_type` doesn't reach the cached AST clones); falls back to the
/// typecheck-populated `resolved_type` and finally to the variable-binding
/// heuristic. Always returns a usable type when any source has one; only
/// returns `Type::Unknown` when every source agrees there is none.
///
/// `var_type` looks a binding name up in the surrounding LLVM-bound
/// variables map (which expo-ir cannot reach into directly because that
/// map's value carries `BasicValueEnum<'ctx>`).
pub fn resolve_subject_ty(
    ctx: &LowerCtx<'_>,
    subject: &Expr,
    post_emit_ty: &Type,
    var_type: impl Fn(&str) -> Option<Type>,
) -> Type {
    if !matches!(post_emit_ty, Type::Unknown) {
        return post_emit_ty.clone();
    }
    if let Some(ty) = subject.resolved_type.as_ref() {
        let substituted = monomorphize_type(ctx, ty);
        if !matches!(substituted, Type::Unknown) {
            return substituted;
        }
    }
    if let ExprKind::Ident { name, .. } = &subject.kind
        && let Some(ty) = var_type(name)
    {
        return ty;
    }
    if matches!(subject.kind, ExprKind::Self_)
        && let Some(ty) = var_type("self")
    {
        return ty;
    }
    Type::Unknown
}

/// Lowers a match expression to a [`ResolvedMatch`]. The subject type is
/// passed in by the caller (resolved via [`resolve_subject_ty`]); each
/// pattern is resolved via [`lower_pattern`]. The result-type strategy is
/// chosen by the caller (currently `lower_result_ty` in codegen, kept
/// there because it consults `LLVMTypeCache::contains_monomorphized`).
pub fn lower_match(
    ctx: &LowerCtx<'_>,
    subject_ty: &Type,
    arms: &[MatchArm],
    result_ty: ResolvedMatchType,
) -> Result<ResolvedMatch, String> {
    let mut patterns = Vec::with_capacity(arms.len());
    for arm in arms {
        patterns.push(lower_pattern(ctx, &arm.pattern, subject_ty)?);
    }
    Ok(ResolvedMatch {
        subject_ty: subject_ty.clone(),
        patterns,
        result_ty,
    })
}

/// Resolves an AST pattern against the subject's Expo type, producing a
/// `ResolvedPattern` whose enum keys, tags, field indices, and variant
/// shapes have all been validated against the type registry.
pub fn lower_pattern(
    ctx: &LowerCtx<'_>,
    pattern: &Pattern,
    subject_type: &Type,
) -> Result<ResolvedPattern, String> {
    match pattern {
        Pattern::Wildcard { .. } => Ok(ResolvedPattern::AlwaysMatch),

        Pattern::Binding { name, .. } => Ok(ResolvedPattern::Bind {
            name: name.clone(),
            ty: subject_type.clone(),
            strict_llvm: false,
        }),

        Pattern::Literal { value, .. } => Ok(ResolvedPattern::LiteralEq {
            lit: lower_literal(value)?,
            subject_ty: subject_type.clone(),
        }),

        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            Ok(ResolvedPattern::EnumUnit {
                enum_key,
                variant: variant.clone(),
                tag,
            })
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            let elements = lower_tuple_elements(ctx, &enum_key, variant, elements)?;
            Ok(ResolvedPattern::EnumTuple {
                enum_key,
                variant: variant.clone(),
                tag,
                elements,
            })
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            let fields = lower_struct_fields(ctx, &enum_key, variant, fields)?;
            Ok(ResolvedPattern::EnumStruct {
                enum_key,
                variant: variant.clone(),
                tag,
                fields,
            })
        }

        Pattern::Constructor { name, elements, .. } => {
            let enum_key = resolve_enum_key_from_constructor(ctx, name, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, name)?;
            if elements.is_empty() {
                // Constructor with no payload acts as a unit-variant tag check
                // -- collapsing to `EnumUnit` keeps emission's no-payload-GEP
                // invariant uniform.
                Ok(ResolvedPattern::EnumUnit {
                    enum_key,
                    variant: name.clone(),
                    tag,
                })
            } else {
                let elements = lower_tuple_elements(ctx, &enum_key, name, elements)?;
                Ok(ResolvedPattern::EnumTuple {
                    enum_key,
                    variant: name.clone(),
                    tag,
                    elements,
                })
            }
        }

        Pattern::TypedBinding {
            name, type_expr, ..
        } => {
            let resolved = resolve_type_expr(ctx, type_expr);
            let subject_inner = unwrap_indirect(subject_type);

            if mangle_type(&resolved) == mangle_type(subject_inner) {
                Ok(ResolvedPattern::Bind {
                    name: name.clone(),
                    ty: resolved,
                    strict_llvm: true,
                })
            } else {
                let union_mangled = mangle_type(subject_inner);
                let member_mangled = mangle_type(&resolved);
                let tag = union_member_tag(subject_inner, &member_mangled).ok_or_else(|| {
                    format!("unknown union member: {union_mangled}.{member_mangled}")
                })?;
                Ok(ResolvedPattern::UnionMember {
                    union_mangled,
                    member_mangled,
                    tag,
                    member_ty: resolved,
                    bind_name: name.clone(),
                })
            }
        }

        Pattern::List { .. } => Err("list patterns not yet supported in compilation".to_string()),

        Pattern::Binary { segments, .. } => Ok(ResolvedPattern::Binary {
            segments: segments.clone(),
        }),

        Pattern::Or { patterns, .. } => {
            let mut subs = Vec::with_capacity(patterns.len());
            for p in patterns {
                subs.push(lower_pattern(ctx, p, subject_type)?);
            }
            Ok(ResolvedPattern::Or(subs))
        }
    }
}

pub fn lower_literal(lit: &Literal) -> Result<ResolvedLiteral, String> {
    match lit {
        Literal::Bool(b) => Ok(ResolvedLiteral::Bool(*b)),
        Literal::Float(s) => s
            .parse::<f64>()
            .map(ResolvedLiteral::Float)
            .map_err(|_| format!("invalid float: {s}")),
        Literal::Int(s) => parse_int_literal(s).map(ResolvedLiteral::Int),
        Literal::String(s) => Ok(ResolvedLiteral::String(s.clone())),
        Literal::Unit => Err("unsupported literal in match pattern".to_string()),
    }
}

pub fn lower_tuple_elements(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
    elements: &[Pattern],
) -> Result<Vec<(Type, ResolvedPattern)>, String> {
    let field_types = get_tuple_variant_types(ctx, enum_key, variant)?;
    if elements.len() != field_types.len() {
        return Err(format!(
            "variant {enum_key}.{variant} expects {} payload elements, got {}",
            field_types.len(),
            elements.len()
        ));
    }
    let mut out = Vec::with_capacity(elements.len());
    for (sub, ft) in elements.iter().zip(field_types.iter()) {
        let inner = unwrap_indirect(ft).clone();
        let sub_resolved = lower_pattern(ctx, sub, &inner)?;
        out.push((ft.clone(), sub_resolved));
    }
    Ok(out)
}

pub fn lower_struct_fields(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
    fields: &[FieldPattern],
) -> Result<Vec<ResolvedFieldPattern>, String> {
    let expected = get_struct_variant_fields(ctx, enum_key, variant)?;
    let mut out = Vec::with_capacity(fields.len());
    for fp in fields {
        let (idx, (_, field_type)) = expected
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == fp.name)
            .ok_or_else(|| format!("unknown field `{}` in {enum_key}.{variant}", fp.name))?;
        let inner_ty = unwrap_indirect(field_type);
        let sub = match &fp.pattern {
            Some(p) => Some(lower_pattern(ctx, p, inner_ty)?),
            None => None,
        };
        out.push(ResolvedFieldPattern {
            name: fp.name.clone(),
            field_index: idx as u32,
            field_type: field_type.clone(),
            sub,
        });
    }
    Ok(out)
}

/// Resolves the canonical type-cache key for an enum referenced via an AST
/// type path (`Color`, `alpha.Status`, generic-args-bearing `Option<T>`
/// already monomorphized in the subject type, etc.).
pub fn resolve_enum_key_from_path(
    ctx: &LowerCtx<'_>,
    type_path: &[String],
    subject_type: &Type,
) -> Result<String, String> {
    let ty = unwrap_indirect(subject_type);
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Ok(mangle_name(identifier, type_args)),
        Type::Named { identifier, .. } => {
            let name = &identifier.name;
            if let Some((base, _)) = try_parse_mangled_name(ctx, name)
                && ctx.type_ctx.is_enum(&base)
            {
                Ok(name.clone())
            } else if identifier.package != Package::Unresolved {
                Ok(identifier.qualified_name())
            } else if !type_path.is_empty() {
                resolve_enum_key_from_joined(ctx, &type_path.join("."), subject_type)
            } else {
                Err("cannot determine enum name for pattern".to_string())
            }
        }
        _ if !type_path.is_empty() => {
            resolve_enum_key_from_joined(ctx, &type_path.join("."), subject_type)
        }
        _ => Err("cannot determine enum name for pattern".to_string()),
    }
}

/// Resolves the canonical type-cache key for a shorthand constructor
/// pattern (`Some(x)`, `Ok(_)`) -- where the variant name is given without
/// an enum-name qualifier.
pub fn resolve_enum_key_from_constructor(
    ctx: &LowerCtx<'_>,
    variant_name: &str,
    subject_type: &Type,
) -> Result<String, String> {
    let subject_type = unwrap_indirect(subject_type);
    if let Type::Named {
        identifier,
        type_args,
    } = subject_type
    {
        let name = &identifier.name;
        if !type_args.is_empty() {
            return Ok(mangle_name(identifier, type_args));
        }
        if let Some((base, _)) = try_parse_mangled_name(ctx, name)
            && ctx.type_ctx.is_enum(&base)
        {
            return Ok(name.clone());
        }
        if identifier.package != Package::Unresolved
            && ctx
                .type_ctx
                .get_type(identifier)
                .is_some_and(|ti| ti.is_enum())
        {
            return Ok(identifier.qualified_name());
        }
        if ctx.type_ctx.is_enum(name) {
            return Ok(name.clone());
        }
    }
    if let Type::Union(members) = subject_type {
        let member_mangled = mangle_type(&named(variant_name));
        if members.iter().any(|m| mangle_type(m) == member_mangled) {
            return Ok(mangle_type(subject_type));
        }
    }
    for (enum_name, info) in ctx.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if info
            .variants()
            .is_some_and(|vs| vs.iter().any(|v| v.name == variant_name))
        {
            return Ok(enum_name.name.clone());
        }
    }
    Err(format!("no enum found with variant `{variant_name}`"))
}

fn resolve_enum_key_from_joined(
    ctx: &LowerCtx<'_>,
    joined: &str,
    subject_type: &Type,
) -> Result<String, String> {
    if let Some(id) = resolve_name_current(ctx, joined) {
        let qualified = id.qualified_name();
        if ctx.type_ctx.get_type(id).is_some()
            || ctx.layouts.contains_monomorphized(&qualified)
            || ctx.layouts.contains_enum(&qualified)
        {
            return Ok(qualified);
        }
    }
    let bare_id = TypeIdentifier::from_qualified_name(joined);
    if ctx.type_ctx.get_type(&bare_id).is_some()
        || ctx.layouts.contains_monomorphized(joined)
        || ctx.layouts.contains_enum(joined)
    {
        return Ok(joined.to_string());
    }
    Err(format!(
        "cannot resolve enum name from pattern `{joined}` for match subject type `{}`",
        subject_type.display()
    ))
}

fn lookup_variant_tag(ctx: &LowerCtx<'_>, enum_key: &str, variant: &str) -> Result<u8, String> {
    ctx.layouts
        .variant_index(enum_key, variant)
        .ok_or_else(|| format!("unknown variant: {enum_key}.{variant}"))
}

/// Tag (= position) of a union member, derived directly from the union's
/// member list. Unions do not flow through `TypeLayouts` or `LLVMTypeCache`
/// — their tag and payload are fully determined by the surrounding
/// `Type::Union(members)` at the use site.
fn union_member_tag(union_ty: &Type, member_mangled: &str) -> Option<u8> {
    let Type::Union(members) = union_ty else {
        return None;
    };
    members
        .iter()
        .position(|m| mangle_type(m) == member_mangled)
        .map(|i| i as u8)
}

fn get_struct_variant_fields(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(ctx, enum_key, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_key}.{variant} is not a struct variant")),
    }
}

fn get_tuple_variant_types(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(ctx, enum_key, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_key}.{variant} is not a tuple variant")),
    }
}

fn lookup_variant_data(
    ctx: &LowerCtx<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<VariantData, String> {
    if let Some(ti) = find_type_current(ctx, enum_name)
        && let Some(vs) = ti.variants()
        && let Some(vi) = vs.iter().find(|v| v.name == variant)
    {
        return Ok(vi.data.clone());
    }
    if let Some(variants) = ctx.layouts.enum_variants(enum_name)
        && let Some((_, data)) = variants.iter().find(|(n, _)| n == variant)
    {
        return Ok(data.clone());
    }
    Err(format!("variant not found: {enum_name}.{variant}"))
}
