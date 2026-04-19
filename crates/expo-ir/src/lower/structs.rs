//! Lowering for concrete struct construction.
//!
//! Resolves the mangled struct name (post-monomorphization for generics)
//! and the per-field layout in initializer order; emission then walks the
//! [`crate::resolved::construction::ResolvedStructConstruction`] to do
//! GEP/store.

use expo_ast::ast::FieldInit;
use expo_typecheck::types::{
    Package, Type, TypeIdentifier, resolve_type_alias_id, resolve_type_alias_name,
};

use crate::lower::ctx::LowerCtx;
use crate::lower::types::resolve_name_current;
use crate::resolved::construction::ResolvedStructConstruction;
use crate::resolved::fields::ResolvedStructField;

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
