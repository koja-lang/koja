//! Lowering helpers for struct field access.
//!
//! These functions are the read-side counterpart to the data types in
//! [`crate::resolved::fields`]. Given a struct's resolved [`Type`] (or a
//! [`TypeIdentifier`] for the strict, non-generic case), they pick a
//! collision-safe lookup path and return a [`ResolvedFieldStep`] the
//! emission code can use directly.
//!
//! All three functions take only semantic context (`&TypeContext`,
//! `&TypeLayouts`) — they have no dependency on `expo-codegen` or LLVM and
//! could equally be consumed by a future Cranelift, C, or WASM backend.
//! They are the first lowering functions hosted in `expo-ir`.

use expo_ast::identifier::{Package, TypeIdentifier};
use expo_ast::types::{Type, mangle_name};
use expo_typecheck::context::TypeContext;

use crate::TypeLayouts;
use crate::resolved::fields::ResolvedFieldStep;

/// Strict lookup for a non-generic struct's field index by
/// [`TypeIdentifier`]. A `Package::Unresolved` identifier returns `None`
/// rather than masking bugs with a last-write-wins resolution.
pub fn concrete_field_index(
    type_ctx: &TypeContext,
    id: &TypeIdentifier,
    field_name: &str,
) -> Option<u32> {
    let info = type_ctx.get_type(id)?;
    let fields = info.fields()?;
    fields
        .iter()
        .position(|(name, _)| name == field_name)
        .map(|i| i as u32)
}

/// Strict counterpart of [`concrete_field_index`] that returns the field
/// type.
pub fn concrete_field_type(
    type_ctx: &TypeContext,
    id: &TypeIdentifier,
    field_name: &str,
) -> Option<Type> {
    let info = type_ctx.get_type(id)?;
    let fields = info.fields()?;
    fields
        .iter()
        .find(|(name, _)| name == field_name)
        .map(|(_, ty)| ty.clone())
}

/// Look up a struct's field index and type, dispatching on the struct's
/// resolved [`Type`] to pick a collision-safe lookup path:
///
///   * Non-generic `Type::Named` with a resolved package → strict
///     TypeIdentifier-keyed lookup via [`concrete_field_index`] /
///     [`concrete_field_type`].
///   * Generic `Type::Named` → mangled lookup via [`TypeLayouts`].
///   * `Type::Indirect` / `Type::Pointer` → recursively unwrap.
///
/// Unresolved identifiers return `None`: callers are expected to thread a
/// package-qualified `TypeIdentifier` from typecheck, not a bare name.
pub fn lower_struct_field(
    layouts: &TypeLayouts,
    type_ctx: &TypeContext,
    ty: &Type,
    field_name: &str,
) -> Option<ResolvedFieldStep> {
    match ty {
        Type::Indirect(inner) | Type::Pointer(inner) => {
            lower_struct_field(layouts, type_ctx, inner, field_name)
        }
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let mangled = mangle_name(identifier, type_args);
            let field_index = layouts.field_index(&mangled, field_name)?;
            let field_type = layouts.field_type(&mangled, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        Type::Named { identifier, .. } if identifier.package != Package::Unresolved => {
            let field_index = concrete_field_index(type_ctx, identifier, field_name)?;
            let field_type = concrete_field_type(type_ctx, identifier, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        // Flattened form: generic Named with empty type_args where
        // `identifier.name` already holds the mangled key.
        Type::Named { identifier, .. } => {
            let field_index = layouts.field_index(&identifier.name, field_name)?;
            let field_type = layouts.field_type(&identifier.name, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        _ => None,
    }
}
