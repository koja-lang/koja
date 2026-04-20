//! Lowering for concrete struct construction.
//!
//! Resolves the mangled struct name (post-monomorphization for generics)
//! and the per-field layout in initializer order; emission then walks the
//! [`crate::resolved::construction::ResolvedStructConstruction`] to do
//! GEP/store.

use expo_ast::ast::{Expr, ExprKind, FieldInit};
use expo_typecheck::types::{
    Package, Type, TypeIdentifier, mangle_name, resolve_type_alias_id, resolve_type_alias_name,
};

use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::resolve_name_current;
use crate::resolved::construction::ResolvedStructConstruction;
use crate::resolved::fields::{ResolvedStructField, ResolvedStructName};

/// Lowers a concrete (non-generic) struct construction.
///
/// Prefers the type-checker's resolved identifier when it carries a
/// resolved package -- that disambiguates two packages that share a struct
/// name without consulting the shared bare-name index.
pub fn lower_concrete_struct(
    ctx: &LowerCtx<'_>,
    raw_name: &str,
    field_inits: &[FieldInit],
    resolved_type: Option<&TypeIdentifier>,
) -> Result<ResolvedStructConstruction, String> {
    let struct_name = resolve_type_alias_name(raw_name, &ctx.type_ctx.type_aliases);

    let lookup_id = resolved_type
        .filter(|id| id.package != Package::Unresolved)
        .cloned()
        .or_else(|| resolve_type_alias_id(raw_name, &ctx.type_ctx.type_aliases))
        .or_else(|| resolve_name_current(ctx, &struct_name).cloned())
        .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

    let struct_fields = ctx
        .type_ctx
        .get_type(&lookup_id)
        .filter(|ti| ti.is_struct())
        .ok_or_else(|| format!("unknown struct: {struct_name}"))?
        .fields()
        .ok_or_else(|| format!("internal: `{struct_name}` is not a struct"))?;

    let mut fields = Vec::with_capacity(field_inits.len());
    for field_init in field_inits {
        let (idx, field_type) = struct_fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == &field_init.name)
            .map(|(i, (_, ty))| (i as u32, ty.clone()))
            .ok_or_else(|| {
                format!(
                    "unknown field `{}` in struct `{}`",
                    field_init.name, struct_name
                )
            })?;
        fields.push(ResolvedStructField {
            field_type,
            index: idx,
            name: field_init.name.clone(),
        });
    }

    Ok(ResolvedStructConstruction {
        fields,
        is_generic: false,
        mangled_name: lookup_id.qualified_name(),
        result_type: Type::Named {
            identifier: lookup_id,
            type_args: vec![],
        },
    })
}

/// Resolves the [`ResolvedStructName`] for a method-call receiver,
/// trying three sources in order: the receiver's static Expo type, the
/// type recorded for the receiver variable in the surrounding scope,
/// and a caller-provided LLVM struct name fallback (the only path
/// that needs information from emission).
///
/// `var_type` looks the receiver name up in the LLVM-bound variables
/// map -- the same closure pattern that
/// [`crate::lower::stmt::resolve_field_path`] uses, so emission stays
/// the only place that touches the per-function `BasicValueEnum`/Type
/// pairs. `llvm_struct_name` is the receiver value's LLVM struct name
/// when it is a struct value, precomputed by the caller.
pub fn resolve_struct_name(
    ctx: &LowerCtx<'_>,
    receiver: &Expr,
    recv_type: &Type,
    var_type: impl Fn(&str) -> Option<Type>,
    llvm_struct_name: Option<&str>,
) -> Result<ResolvedStructName, String> {
    let mut result = struct_name_from_type(recv_type);

    if result.is_none()
        && let ExprKind::Ident { name, .. } = &receiver.kind
        && let Some(ty) = var_type(name)
    {
        result = struct_name_from_type(&ty);
    }

    if result.is_none()
        && let Some(name) = llvm_struct_name
    {
        let identifier = resolve_name_current(ctx, name).cloned();
        result = Some(ResolvedStructName {
            base: name.to_string(),
            identifier,
            mangled: name.to_string(),
            type_args: vec![],
        });
    }

    let mut sn = result.ok_or("cannot determine struct type for method call")?;

    if sn.type_args.is_empty()
        && let Some((base, type_args)) = try_parse_mangled_name(ctx, &sn.mangled)
    {
        sn.identifier = resolve_name_current(ctx, &base).cloned();
        sn.base = base;
        sn.type_args = type_args;
    }

    Ok(sn)
}

fn struct_name_from_type(ty: &Type) -> Option<ResolvedStructName> {
    match ty {
        Type::Indirect(inner) => struct_name_from_type(inner),
        Type::Pointer(inner) => {
            let cptr_id = TypeIdentifier::std("CPtr");
            let mangled = mangle_name(&cptr_id, &[*inner.clone()]);
            Some(ResolvedStructName {
                base: cptr_id.name.clone(),
                identifier: Some(cptr_id),
                mangled,
                type_args: vec![*inner.clone()],
            })
        }
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Some(ResolvedStructName {
            base: identifier.name.clone(),
            identifier: Some(identifier.clone()),
            mangled: mangle_name(identifier, type_args),
            type_args: type_args.clone(),
        }),
        Type::Named { identifier, .. } => Some(ResolvedStructName {
            base: identifier.name.clone(),
            identifier: Some(identifier.clone()),
            mangled: identifier.name.clone(),
            type_args: vec![],
        }),
        Type::Primitive(p) => {
            let name = p.display().to_string();
            Some(ResolvedStructName {
                base: name.clone(),
                identifier: None,
                mangled: name,
                type_args: vec![],
            })
        }
        _ => None,
    }
}
