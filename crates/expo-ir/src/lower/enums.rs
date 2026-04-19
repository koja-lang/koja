//! Lowering for concrete enum construction and equality decisions.
//!
//! Resolves the mangled enum name (post-monomorphization for generics),
//! picks the right variant tag, and computes the per-variant field shape.
//! Pure semantic work -- no LLVM types or values are produced here.

use std::collections::HashMap;

use expo_ast::ast::{EnumConstructionData, TypeParam};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, TypeIdentifier, mangle_name, unwrap_indirect};

use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::resolved::construction::ResolvedEnumConstruction;
use crate::resolved::enums::{ResolvedEnumEq, ResolvedVariantEq, ResolvedVariantFields};

/// Resolved mangled name for an enum type, or `None` if `ty` doesn't refer
/// to an enum-shaped `Type::Named`. Used by both equality compilation and
/// the type-args resolver to spot enum subjects.
pub fn enum_mangled_name(ty: &Type) -> Option<String> {
    match unwrap_indirect(ty) {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Some(mangle_name(identifier, type_args)),
        Type::Named { identifier, .. } => Some(identifier.qualified_name()),
        _ => None,
    }
}

/// Lowers a concrete (non-generic) enum construction.
///
/// Validates the variant tag against the layout table and resolves the
/// per-variant field shape via [`lower_concrete_variant_fields`]. Replaces
/// `expo_codegen::enums::lower_concrete_enum`.
pub fn lower_concrete_enum(
    ctx: &LowerCtx<'_>,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_id: Option<TypeIdentifier>,
) -> Result<ResolvedEnumConstruction, String> {
    let resolved_id = resolved_id.ok_or_else(|| format!("unknown enum type: {enum_name}"))?;

    let key = resolved_id.qualified_name();
    let tag = ctx
        .layouts
        .variant_index(&key, variant)
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{resolved_id}`"))?
        as u64;

    let variant_fields = lower_concrete_variant_fields(ctx, &resolved_id, variant, data)?;

    Ok(ResolvedEnumConstruction {
        is_generic: false,
        mangled_name: key,
        result_type: Type::Named {
            identifier: resolved_id,
            type_args: vec![],
        },
        tag,
        variant_fields,
        variant_name: variant.to_string(),
    })
}

/// Resolves the per-variant field shape for a concrete enum construction
/// from typecheck-supplied variant data.
pub fn lower_concrete_variant_fields(
    ctx: &LowerCtx<'_>,
    enum_id: &TypeIdentifier,
    variant: &str,
    data: &EnumConstructionData,
) -> Result<ResolvedVariantFields, String> {
    let variant_data = ctx
        .type_ctx
        .get_type(enum_id)
        .and_then(|ti| ti.variants())
        .and_then(|vs| vs.iter().find(|v| v.name == variant))
        .map(|vi| vi.data.clone());

    match data {
        EnumConstructionData::Struct(field_inits) => {
            let expected = match variant_data {
                Some(VariantData::Struct(f)) => f,
                _ => return Err(format!("{enum_id}.{variant} is not a struct variant")),
            };
            let mut fields = Vec::with_capacity(field_inits.len());
            for field_init in field_inits {
                let (idx, field_type) = expected
                    .iter()
                    .enumerate()
                    .find(|(_, (name, _))| name == &field_init.name)
                    .map(|(i, (_, ty))| (i as u32, ty.clone()))
                    .ok_or_else(|| {
                        format!("unknown field `{}` in {enum_id}.{variant}", field_init.name)
                    })?;
                fields.push((field_init.name.clone(), idx, field_type));
            }
            Ok(ResolvedVariantFields::Struct { fields })
        }
        EnumConstructionData::Tuple(_) => {
            let element_types = match variant_data {
                Some(VariantData::Tuple(types)) => types,
                _ => Vec::new(),
            };
            Ok(ResolvedVariantFields::Tuple { element_types })
        }
        EnumConstructionData::Unit => Ok(ResolvedVariantFields::Unit),
    }
}

/// Resolves the type-argument vector for a generic enum construction by
/// consulting (in order) the unify-derived `subst`, the surrounding
/// function's `type_subst`, and finally the function's union return-type
/// hint when any slot is still `Type::Unknown`.
pub fn resolve_generic_type_args(
    ctx: &LowerCtx<'_>,
    type_params: &[TypeParam],
    subst: &HashMap<String, Type>,
    enum_name: &str,
) -> Vec<Type> {
    let mut type_args: Vec<Type> = type_params
        .iter()
        .map(|tp| {
            subst
                .get(&tp.name)
                .cloned()
                .or_else(|| ctx.fn_lower.type_subst.get(&tp.name).cloned())
                .unwrap_or(Type::Unknown)
        })
        .collect();

    if !type_args.contains(&Type::Unknown) {
        return type_args;
    }

    let Some(hint) = ctx.fn_lower.return_type_hint.as_ref() else {
        return type_args;
    };

    let hint_args = match hint {
        Type::Named {
            identifier,
            type_args: ha,
        } if identifier.name == enum_name && !ha.is_empty() => Some(ha.clone()),
        Type::Named { identifier, .. } => try_parse_mangled_name(ctx, &identifier.name)
            .filter(|(base, _)| base == enum_name)
            .map(|(_, ha)| ha),
        _ => None,
    };

    if let Some(ha) = hint_args {
        for (i, ta) in type_args.iter_mut().enumerate() {
            if *ta == Type::Unknown && i < ha.len() {
                *ta = ha[i].clone();
            }
        }
    }
    type_args
}

/// Resolves the per-variant equality decision table for an enum-typed value.
/// Replaces `expo_codegen::enums::resolve_enum_eq`.
pub fn resolve_enum_eq(ctx: &LowerCtx<'_>, ty: &Type) -> Result<ResolvedEnumEq, String> {
    let mangled = enum_mangled_name(ty)
        .ok_or_else(|| "compile_enum_struct_eq called with non-enum type".to_string())?;

    let registered = ctx
        .layouts
        .enum_variants(&mangled)
        .ok_or_else(|| format!("enum variants not found for `{mangled}`"))?;

    let mut variants = Vec::with_capacity(registered.len());
    for (name, vdata) in registered {
        let resolved = match vdata {
            VariantData::Struct(fields) => ResolvedVariantEq::Struct {
                field_types: fields.iter().map(|(_, t)| t.clone()).collect(),
            },
            VariantData::Tuple(types) => ResolvedVariantEq::Tuple {
                field_types: types.clone(),
            },
            VariantData::Unit => ResolvedVariantEq::Unit,
        };
        variants.push((name.clone(), resolved));
    }

    Ok(ResolvedEnumEq { mangled, variants })
}
