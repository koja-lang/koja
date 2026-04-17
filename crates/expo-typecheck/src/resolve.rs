use std::collections::{BTreeMap, BTreeSet};

use crate::context::{FunctionSig, TypeContext, TypeKind, VariantData};
use crate::types::{Package, Type, TypeIdentifier};
use expo_ast::ast::{Diagnostic, Severity};

/// Walks every `Type` in a [`TypeContext`] and replaces `Package::Unresolved`
/// identifiers with the real package found in the type registry's map keys.
///
/// Must be called after collection and merging, before checking. At that point
/// the map keys carry real packages (set by `collect_module`) while most
/// `Type::Named` identifiers still carry `Package::Unresolved` from the type
/// expression resolver. This pass bridges that gap.
///
/// Bare entries in `name_index` are restricted to `std` types so a bare
/// reference to `X` resolves only to `{current_package}.X` (via the qualified
/// entry) or `std.X` (via the bare entry). Cross-package bare references to
/// dependency types must be qualified (`dep.X`) or imported via `alias`.
pub fn resolve_packages(ctx: &mut TypeContext, _dep_packages: &[String]) {
    let mut index: BTreeMap<String, TypeIdentifier> = BTreeMap::new();
    for id in ctx.types.keys() {
        index.insert(id.qualified_name(), id.clone());
        if id.package == Package::Std {
            index.insert(id.name.clone(), id.clone());
        }
    }
    // Aliases let a module use a short local name for a type owned by another
    // package (`alias json.StringBuilder` → `StringBuilder` ↦ `json.StringBuilder`).
    // Seed the resolution index with each alias so collected signatures whose
    // bodies were never type-checked (embedded stdlib/lib modules) can still
    // upgrade their `Package::Unresolved` references to the right package.
    for (local_name, ty) in &ctx.type_aliases {
        if index.contains_key(local_name) {
            continue;
        }
        if let Type::Named { identifier, .. } = ty
            && identifier.package != Package::Unresolved
        {
            index.insert(local_name.clone(), identifier.clone());
        }
    }

    let type_keys: Vec<TypeIdentifier> = ctx.types.keys().cloned().collect();
    for key in type_keys {
        let scope = key.package.clone();
        if let Some(ti) = ctx.types.get_mut(&key) {
            resolve_identifier_scoped(&mut ti.identifier, &index, &scope);
            resolve_type_kind_scoped(&mut ti.kind, &index, &scope);
            resolve_function_sigs_scoped(&mut ti.functions, &index, &scope);
        }
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

    let impl_keys: Vec<TypeIdentifier> = ctx.protocol_impls.keys().cloned().collect();
    for key in impl_keys {
        let scope = key.package.clone();
        if let Some(impls) = ctx.protocol_impls.get_mut(&key) {
            for (_, type_args) in impls {
                for ty in type_args {
                    resolve_type_scoped(ty, &index, &scope);
                }
            }
        }
    }

    for pi in ctx.protocols.values_mut() {
        resolve_function_sigs(&mut pi.methods, &index);
    }

    let spec_keys: Vec<TypeIdentifier> = ctx.specialized_methods.keys().cloned().collect();
    for mut key in spec_keys {
        if let Some(mut entries) = ctx.specialized_methods.remove(&key) {
            let scope = key.package.clone();
            for (type_args, sigs) in &mut entries {
                for ty in type_args.iter_mut() {
                    resolve_type_scoped(ty, &index, &scope);
                }
                resolve_function_sigs_scoped(sigs, &index, &scope);
            }
            resolve_identifier_scoped(&mut key, &index, &scope);
            ctx.specialized_methods
                .entry(key)
                .or_default()
                .extend(entries);
        }
    }

    let spec_ast_keys: Vec<TypeIdentifier> = ctx.specialized_impl_asts.keys().cloned().collect();
    for mut key in spec_ast_keys {
        if let Some(mut entries) = ctx.specialized_impl_asts.remove(&key) {
            let scope = key.package.clone();
            for (type_args, _) in &mut entries {
                for ty in type_args.iter_mut() {
                    resolve_type_scoped(ty, &index, &scope);
                }
            }
            resolve_identifier_scoped(&mut key, &index, &scope);
            ctx.specialized_impl_asts
                .entry(key)
                .or_default()
                .extend(entries);
        }
    }

    let std_names: BTreeSet<&str> = ctx
        .types
        .keys()
        .filter(|id| id.package == Package::Std)
        .map(|id| id.name.as_str())
        .collect();
    let shadow_errors: Vec<_> = ctx
        .types
        .iter()
        .filter(|(id, _)| id.package != Package::Std && std_names.contains(id.name.as_str()))
        .map(|(id, ti)| Diagnostic {
            severity: Severity::Error,
            message: format!(
                "type `{}` conflicts with stdlib type of the same name",
                id.name
            ),
            hint: None,
            span: ti.span,
        })
        .collect();
    ctx.diagnostics.extend(shadow_errors);

    ctx.name_index = index;

    let mut pkg_types: BTreeMap<Package, BTreeSet<String>> = BTreeMap::new();
    for id in ctx.types.keys() {
        pkg_types
            .entry(id.package.clone())
            .or_default()
            .insert(id.name.clone());
    }
    ctx.package_types = pkg_types;
}

/// Resolves `Package::Unresolved` identifiers in a single [`Type`] using a
/// pre-built name-to-identifier index. Used during checking when new types are
/// constructed from AST type expressions after the bulk resolution pass.
pub fn resolve_type_inline(ty: &mut Type, index: &BTreeMap<String, TypeIdentifier>) {
    resolve_type(ty, index);
}

/// Scope-aware counterpart of [`resolve_type_inline`]. Prefers resolutions in
/// `scope` (via the qualified `scope.name` entry) before consulting the shared
/// bare entry, matching the behavior of [`crate::TypeContext::find_type`] when
/// a current package is active.
pub fn resolve_type_inline_scoped(
    ty: &mut Type,
    index: &BTreeMap<String, TypeIdentifier>,
    scope: &Package,
) {
    resolve_type_scoped(ty, index, scope);
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

/// Scope-aware counterpart of [`resolve_identifier`]. When a bare name is
/// ambiguous across packages, resolution inside a `scope` package prefers the
/// same-package definition (via the qualified `"scope.name"` entry) before
/// falling back to the shared bare entry.
fn resolve_identifier_scoped(
    id: &mut TypeIdentifier,
    index: &BTreeMap<String, TypeIdentifier>,
    scope: &Package,
) {
    if id.package != Package::Unresolved {
        return;
    }
    let qualified = match scope {
        Package::Std => format!("std.{}", id.name),
        Package::Named(pkg) => format!("{pkg}.{}", id.name),
        Package::Unresolved => {
            if let Some(resolved) = index.get(&id.name) {
                id.package = resolved.package.clone();
            }
            return;
        }
    };
    if let Some(resolved) = index.get(&qualified) {
        id.package = resolved.package.clone();
    } else if let Some(resolved) = index.get(&id.name) {
        id.package = resolved.package.clone();
    }
}

fn resolve_type_scoped(ty: &mut Type, index: &BTreeMap<String, TypeIdentifier>, scope: &Package) {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            resolve_identifier_scoped(identifier, index, scope);
            for arg in type_args {
                resolve_type_scoped(arg, index, scope);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for p in params {
                resolve_type_scoped(&mut p.ty, index, scope);
            }
            resolve_type_scoped(return_type, index, scope);
        }
        Type::Indirect(inner) | Type::Pointer(inner) => resolve_type_scoped(inner, index, scope),
        Type::Union(members) => {
            for m in members {
                resolve_type_scoped(m, index, scope);
            }
        }
        Type::Primitive(_) | Type::Parameter(_) | Type::Unit | Type::Unknown | Type::Error => {}
    }
}

fn resolve_type_kind_scoped(
    kind: &mut TypeKind,
    index: &BTreeMap<String, TypeIdentifier>,
    scope: &Package,
) {
    match kind {
        TypeKind::Struct { fields } => {
            for (_, ty) in fields {
                resolve_type_scoped(ty, index, scope);
            }
        }
        TypeKind::Enum { variants } => {
            for vi in variants {
                match &mut vi.data {
                    VariantData::Struct(fields) => {
                        for (_, ty) in fields {
                            resolve_type_scoped(ty, index, scope);
                        }
                    }
                    VariantData::Tuple(types) => {
                        for ty in types {
                            resolve_type_scoped(ty, index, scope);
                        }
                    }
                    VariantData::Unit => {}
                }
            }
        }
        TypeKind::Primitive => {}
    }
}

fn resolve_function_sigs_scoped(
    fns: &mut BTreeMap<String, FunctionSig>,
    index: &BTreeMap<String, TypeIdentifier>,
    scope: &Package,
) {
    for sig in fns.values_mut() {
        for p in &mut sig.params {
            resolve_type_scoped(&mut p.ty, index, scope);
        }
        resolve_type_scoped(&mut sig.return_type, index, scope);
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
