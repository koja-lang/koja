use std::collections::BTreeMap;

use crate::context::{FunctionSig, TypeContext, TypeKind, VariantData};
use crate::types::{Package, Type, TypeIdentifier};

/// Walks every `Type` in a [`TypeContext`] and replaces `Package::Unresolved`
/// identifiers with the real package found in the type registry's map keys.
///
/// Must be called after collection and merging, before checking. At that point
/// the map keys carry real packages (set by `collect_module`) while most
/// `Type::Named` identifiers still carry `Package::Unresolved` from the type
/// expression resolver. This pass bridges that gap.
pub fn resolve_packages(ctx: &mut TypeContext) {
    let index: BTreeMap<String, TypeIdentifier> = ctx
        .types
        .keys()
        .map(|id| (id.name.clone(), id.clone()))
        .collect();

    for ti in ctx.types.values_mut() {
        resolve_identifier(&mut ti.identifier, &index);
        resolve_type_kind(&mut ti.kind, &index);
        resolve_function_sigs(&mut ti.functions, &index);
    }

    resolve_function_sigs(&mut ctx.functions, &index);

    for ty in ctx.constants.values_mut() {
        resolve_type(ty, &index);
    }

    for ty in ctx.type_aliases.values_mut() {
        resolve_type(ty, &index);
    }

    for ty in ctx.module_aliases.values_mut() {
        resolve_type(ty, &index);
    }

    for impls in ctx.protocol_impls.values_mut() {
        for (_, type_args) in impls {
            for ty in type_args {
                resolve_type(ty, &index);
            }
        }
    }

    for pi in ctx.protocols.values_mut() {
        resolve_function_sigs(&mut pi.methods, &index);
    }

    let spec_keys: Vec<TypeIdentifier> = ctx.specialized_methods.keys().cloned().collect();
    for mut key in spec_keys {
        if let Some(mut entries) = ctx.specialized_methods.remove(&key) {
            for (type_args, sigs) in &mut entries {
                for ty in type_args.iter_mut() {
                    resolve_type(ty, &index);
                }
                resolve_function_sigs(sigs, &index);
            }
            resolve_identifier(&mut key, &index);
            ctx.specialized_methods
                .entry(key)
                .or_default()
                .extend(entries);
        }
    }

    let spec_ast_keys: Vec<TypeIdentifier> = ctx.specialized_impl_asts.keys().cloned().collect();
    for mut key in spec_ast_keys {
        if let Some(mut entries) = ctx.specialized_impl_asts.remove(&key) {
            for (type_args, _) in &mut entries {
                for ty in type_args.iter_mut() {
                    resolve_type(ty, &index);
                }
            }
            resolve_identifier(&mut key, &index);
            ctx.specialized_impl_asts
                .entry(key)
                .or_default()
                .extend(entries);
        }
    }

    ctx.name_index = index;
}

/// Resolves `Package::Unresolved` identifiers in a single [`Type`] using a
/// pre-built name-to-identifier index. Used during checking when new types are
/// constructed from AST type expressions after the bulk resolution pass.
pub fn resolve_type_inline(ty: &mut Type, index: &BTreeMap<String, TypeIdentifier>) {
    resolve_type(ty, index);
}

fn resolve_type(ty: &mut Type, index: &BTreeMap<String, TypeIdentifier>) {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            resolve_identifier(identifier, index);
            for arg in type_args {
                resolve_type(arg, index);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for p in params {
                resolve_type(&mut p.ty, index);
            }
            resolve_type(return_type, index);
        }
        Type::Indirect(inner) | Type::Pointer(inner) => resolve_type(inner, index),
        Type::Union(members) => {
            for m in members {
                resolve_type(m, index);
            }
        }
        Type::Primitive(_) | Type::Parameter(_) | Type::Unit | Type::Unknown | Type::Error => {}
    }
}

fn resolve_identifier(id: &mut TypeIdentifier, index: &BTreeMap<String, TypeIdentifier>) {
    if id.package == Package::Unresolved
        && let Some(resolved) = index.get(&id.name)
    {
        id.package = resolved.package.clone();
    }
}

fn resolve_type_kind(kind: &mut TypeKind, index: &BTreeMap<String, TypeIdentifier>) {
    match kind {
        TypeKind::Struct { fields } => {
            for (_, ty) in fields {
                resolve_type(ty, index);
            }
        }
        TypeKind::Enum { variants } => {
            for vi in variants {
                match &mut vi.data {
                    VariantData::Struct(fields) => {
                        for (_, ty) in fields {
                            resolve_type(ty, index);
                        }
                    }
                    VariantData::Tuple(types) => {
                        for ty in types {
                            resolve_type(ty, index);
                        }
                    }
                    VariantData::Unit => {}
                }
            }
        }
        TypeKind::Primitive => {}
    }
}

fn resolve_function_sigs(
    fns: &mut BTreeMap<String, FunctionSig>,
    index: &BTreeMap<String, TypeIdentifier>,
) {
    for sig in fns.values_mut() {
        for p in &mut sig.params {
            resolve_type(&mut p.ty, index);
        }
        resolve_type(&mut sig.return_type, index);
    }
}
