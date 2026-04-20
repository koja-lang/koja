//! Lowering helpers for struct field access.
//!
//! These functions are the read-side counterpart to the data types in
//! [`crate::resolved::fields`]. Given a struct's resolved [`Type`] (or a
//! [`TypeIdentifier`] for the strict, non-generic case), they pick a
//! collision-safe lookup path and return a [`ResolvedFieldStep`] the
//! emission code can use directly.
//!
//! All three functions take only semantic context (a [`LowerCtx`] borrow
//! bundle, which carries `&TypeContext` and `&TypeLayouts`) — they have no
//! dependency on `expo-codegen` or LLVM and could equally be consumed by a
//! future Cranelift, C, or WASM backend. They are the first lowering
//! functions hosted in `expo-ir`.

use expo_ast::ast::{Expr, ExprKind};
use expo_ast::identifier::{Package, TypeIdentifier};
use expo_ast::types::{Type, mangle_name};

use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::LowerCtx;
use crate::resolved::fields::{ResolvedChain, ResolvedFieldStep};

/// Strict lookup for a non-generic struct's field index by
/// [`TypeIdentifier`]. A `Package::Unresolved` identifier returns `None`
/// rather than masking bugs with a last-write-wins resolution.
pub fn concrete_field_index(
    ctx: &LowerCtx<'_>,
    id: &TypeIdentifier,
    field_name: &str,
) -> Option<u32> {
    let info = ctx.type_ctx.get_type(id)?;
    let fields = info.fields()?;
    fields
        .iter()
        .position(|(name, _)| name == field_name)
        .map(|i| i as u32)
}

/// Strict counterpart of [`concrete_field_index`] that returns the field
/// type.
pub fn concrete_field_type(
    ctx: &LowerCtx<'_>,
    id: &TypeIdentifier,
    field_name: &str,
) -> Option<Type> {
    let info = ctx.type_ctx.get_type(id)?;
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
///   * Generic `Type::Named` → mangled lookup via `ctx.layouts`.
///   * `Type::Indirect` / `Type::Pointer` → recursively unwrap.
///
/// Unresolved identifiers return `None`: callers are expected to thread a
/// package-qualified `TypeIdentifier` from typecheck, not a bare name.
pub fn lower_struct_field(
    ctx: &LowerCtx<'_>,
    ty: &Type,
    field_name: &str,
) -> Option<ResolvedFieldStep> {
    match ty {
        Type::Indirect(inner) | Type::Pointer(inner) => lower_struct_field(ctx, inner, field_name),
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let mangled = MonomorphizedTypeIdentifier::new(mangle_name(identifier, type_args));
            let field_index = ctx.layouts.field_index(&mangled, field_name)?;
            let field_type = ctx.layouts.field_type(&mangled, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        Type::Named { identifier, .. } if identifier.package != Package::Unresolved => {
            let field_index = concrete_field_index(ctx, identifier, field_name)?;
            let field_type = concrete_field_type(ctx, identifier, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        // Flattened form: generic Named with empty type_args where
        // `identifier.name` already holds the mangled key.
        Type::Named { identifier, .. } => {
            let mangled = MonomorphizedTypeIdentifier::new(&identifier.name);
            let field_index = ctx.layouts.field_index(&mangled, field_name)?;
            let field_type = ctx.layouts.field_type(&mangled, field_name)?;
            Some(ResolvedFieldStep {
                field_index,
                field_type,
            })
        }
        _ => None,
    }
}

/// Resolves a static field-access chain rooted at a variable (or `self`)
/// into a base name + base type + sequence of [`ResolvedFieldStep`]s. The
/// chain breaks at any [`Type::Indirect`] step (those need a runtime load
/// rather than a static GEP).
///
/// `var_type` looks a binding name up in the surrounding LLVM-bound
/// variables map (which expo-ir cannot reach into directly because that
/// map's value carries `BasicValueEnum<'ctx>`).
pub fn resolve_chain_steps(
    ctx: &LowerCtx<'_>,
    receiver: &Expr,
    field: &str,
    var_type: &impl Fn(&str) -> Option<Type>,
) -> Option<ResolvedChain> {
    let (base_name, base_type, mut steps) = match &receiver.kind {
        ExprKind::FieldAccess {
            receiver: inner_recv,
            field: inner_field,
            ..
        } => {
            let inner = resolve_chain_steps(ctx, inner_recv, inner_field, var_type)?;
            let last_type = inner
                .steps
                .last()
                .map(|s| &s.field_type)
                .unwrap_or(&inner.base_type);
            if matches!(last_type, Type::Indirect(_)) {
                return None;
            }
            (inner.base_name, inner.base_type, inner.steps)
        }
        ExprKind::Ident { name, .. } => {
            let ty = var_type(name)?;
            (name.clone(), ty, Vec::new())
        }
        ExprKind::Self_ => {
            let ty = var_type("self")?;
            ("self".to_string(), ty, Vec::new())
        }
        _ => return None,
    };

    let current_type = steps.last().map(|s| &s.field_type).unwrap_or(&base_type);
    steps.push(lower_struct_field(ctx, current_type, field)?);

    Some(ResolvedChain {
        base_name,
        base_type,
        steps,
    })
}

/// Identifies struct fields that use [`Type::Indirect`] and returns their
/// indices and types. Checks the monomorphized struct layout first, then
/// falls back to the type context via the package-qualified identifier so
/// cross-package collisions don't return a foreign struct's layout.
pub fn resolve_indirect_field_indices(ctx: &LowerCtx<'_>, ty: &Type) -> Vec<(usize, Type)> {
    let (mono_key, identifier) = match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => (
            Some(MonomorphizedTypeIdentifier::new(mangle_name(
                identifier, type_args,
            ))),
            Some(identifier),
        ),
        Type::Named { identifier, .. } => (
            Some(MonomorphizedTypeIdentifier::new(
                identifier.qualified_name(),
            )),
            Some(identifier),
        ),
        _ => return Vec::new(),
    };

    if let Some(key) = mono_key.as_ref()
        && let Some(fs) = ctx.layouts.struct_layout(key)
    {
        return fs
            .iter()
            .enumerate()
            .filter(|(_, (_, fty))| matches!(fty, Type::Indirect(_)))
            .map(|(i, (_, fty))| (i, fty.clone()))
            .collect();
    }

    if let Some(id) = identifier
        && id.package != Package::Unresolved
        && let Some(ti) = ctx.type_ctx.get_type(id)
        && let Some(fields) = ti.fields()
    {
        return fields
            .iter()
            .enumerate()
            .filter(|(_, (_, fty))| matches!(fty, Type::Indirect(_)))
            .map(|(i, (_, fty))| (i, fty.clone()))
            .collect();
    }

    Vec::new()
}
