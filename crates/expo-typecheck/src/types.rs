use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use expo_ast::ast::{TypeExpr, TypeParam};

pub use expo_ast::identifier::{Package, TypeIdentifier};
pub use expo_ast::types::{
    FnParam, Primitive, Type, contains_parameter, mangle_method_suffix, mangle_name, mangle_type,
    named, numeric_compatible, process_envelope_type, substitute, substitute_preserving, unify,
    unwrap_indirect,
};

/// Converts a caller-facing package label (`"std"` or a real package name such
/// as `"alpha"`) into the matching [`Package`] variant used by the scoped
/// name-index lookup. Empty strings are rejected because every module must
/// carry a real package so bare-name lookups have a deterministic scope.
pub fn package_from_str(package: &str) -> Package {
    assert!(
        !package.is_empty(),
        "package_from_str called with empty package name; callers must supply a real package (file stem, project name, or \"std\")"
    );
    if package == "std" {
        Package::Std
    } else {
        Package::Named(package.to_string())
    }
}

/// Derives a synthetic package name from a module's on-disk file stem.
/// Modules without a path (e.g. in-memory test fixtures or LSP preview
/// buffers) fall back to `fallback`, so every module still carries a
/// concrete package suitable for [`package_from_str`].
pub fn package_for_path(path: Option<&Path>, fallback: &str) -> String {
    path.and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Extracts the package name from a fully-qualified module name.
/// e.g. `"json.decoder"` → `"json"`, `"my_app.main"` → `"my_app"`,
/// `"json"` → `"json"` (single-segment FQN).
pub fn fqn_to_package(fqn: &str) -> &str {
    fqn.split('.').next().unwrap_or(fqn)
}

/// Converts an AST type expression into a resolved [`Type`], looking up user-defined
/// struct and enum names from the provided slices. `known_packages` is the
/// set of package labels the resolver may use to validate qualified `pkg.Type`
/// paths; pass an empty set when no cross-package context is available.
pub fn resolve_type_expr(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_packages: &BTreeSet<Package>,
) -> Type {
    resolve_type_expr_with_params(
        type_expr,
        known_structs,
        known_enums,
        &[],
        &BTreeMap::new(),
        known_packages,
    )
}

/// Checks both global type aliases and file-private module aliases for a name.
pub fn resolve_alias(
    name: &str,
    type_aliases: &BTreeMap<String, Type>,
    module_aliases: &BTreeMap<String, Type>,
) -> Option<Type> {
    module_aliases
        .get(name)
        .or_else(|| type_aliases.get(name))
        .cloned()
}

/// If `name` is a type alias pointing to a named type, returns the underlying
/// type name. Otherwise returns `name` unchanged. Used by the type checker
/// and codegen to resolve aliases before type-info lookup.
pub fn resolve_type_alias_name(name: &str, type_aliases: &BTreeMap<String, Type>) -> String {
    type_aliases
        .get(name)
        .and_then(|ty| match ty {
            Type::Named { identifier, .. } => Some(identifier.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| name.to_string())
}

/// Like [`resolve_type_alias_name`] but returns the full [`TypeIdentifier`]
/// (preserving the package) so callers can do package-aware lookups via
/// [`TypeContext::get_type`]. Returns `None` when no alias matches.
pub fn resolve_type_alias_id(
    name: &str,
    type_aliases: &BTreeMap<String, Type>,
) -> Option<TypeIdentifier> {
    type_aliases.get(name).and_then(|ty| match ty {
        Type::Named { identifier, .. } => Some(identifier.clone()),
        _ => None,
    })
}

/// Builds the `Package` value for a 2-segment path's leading segment.
/// `"std"` maps to [`Package::Std`]; everything else is a [`Package::Named`].
fn path_package(label: &str) -> Package {
    if label == "std" {
        Package::Std
    } else {
        Package::Named(label.to_string())
    }
}

/// Like [`resolve_type_expr`] but also resolves type parameter names (e.g. `T`, `A`)
/// to [`Type::Parameter`] when they appear in generic function/struct definitions,
/// and named type aliases from the provided map.
pub fn resolve_type_expr_with_params(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
    known_packages: &BTreeSet<Package>,
) -> Type {
    resolve_type_expr_full(
        type_expr,
        known_structs,
        known_enums,
        known_type_params,
        known_type_aliases,
        known_packages,
        &BTreeMap::new(),
    )
}

/// Resolves a type expression with full context including the set of known
/// package labels (used to validate qualified `pkg.Type` paths) and
/// file-private module aliases.
pub fn resolve_type_expr_full(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
    known_packages: &BTreeSet<Package>,
    module_aliases: &BTreeMap<String, Type>,
) -> Type {
    match type_expr {
        TypeExpr::Generic { path, args, .. } => {
            if path.len() == 1 && path[0] == "CPtr" && args.len() == 1 {
                let inner = resolve_type_expr_full(
                    &args[0],
                    known_structs,
                    known_enums,
                    known_type_params,
                    known_type_aliases,
                    known_packages,
                    module_aliases,
                );
                return Type::Pointer(Box::new(inner));
            }
            let identifier = if path.len() == 1 {
                let name = path[0].as_str();
                if known_structs.contains(&name) || known_enums.contains(&name) {
                    Some(TypeIdentifier::unresolved(name))
                } else {
                    None
                }
            } else if path.len() == 2 && known_packages.contains(&path_package(&path[0])) {
                Some(qualified_identifier(&path[0], &path[1]))
            } else {
                None
            };
            if let Some(identifier) = identifier {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| {
                        resolve_type_expr_full(
                            a,
                            known_structs,
                            known_enums,
                            known_type_params,
                            known_type_aliases,
                            known_packages,
                            module_aliases,
                        )
                    })
                    .collect();
                return Type::Named {
                    identifier,
                    type_args: resolved_args,
                };
            }
            Type::Unknown
        }
        TypeExpr::Named { path, .. } => {
            if path.len() == 1 {
                let name = path[0].as_str();
                if known_type_params.contains(&name) {
                    return Type::Parameter(name.to_string());
                }
                if let Some(aliased) = resolve_alias(name, known_type_aliases, module_aliases) {
                    return aliased;
                }
                match name {
                    "Binary" => Type::Primitive(Primitive::Binary),
                    "Bits" => Type::Primitive(Primitive::Bits),
                    "String" => Type::Primitive(Primitive::String),
                    "Bool" => Type::Primitive(Primitive::Bool),
                    "Float32" => Type::Primitive(Primitive::F32),
                    "Float" | "Float64" => Type::Primitive(Primitive::F64),
                    "Int8" => Type::Primitive(Primitive::I8),
                    "Int16" => Type::Primitive(Primitive::I16),
                    "Int32" => Type::Primitive(Primitive::I32),
                    "Int" | "Int64" => Type::Primitive(Primitive::I64),
                    "UInt8" => Type::Primitive(Primitive::U8),
                    "UInt16" => Type::Primitive(Primitive::U16),
                    "UInt32" => Type::Primitive(Primitive::U32),
                    "UInt64" => Type::Primitive(Primitive::U64),
                    name => {
                        if known_structs.contains(&name) || known_enums.contains(&name) {
                            Type::Named {
                                identifier: TypeIdentifier::unresolved(name),
                                type_args: vec![],
                            }
                        } else {
                            Type::Unknown
                        }
                    }
                }
            } else if path.len() == 2 && known_packages.contains(&path_package(&path[0])) {
                Type::Named {
                    identifier: qualified_identifier(&path[0], &path[1]),
                    type_args: vec![],
                }
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Self_ { .. } => {
            if known_type_params.contains(&"Self") {
                Type::Parameter("Self".to_string())
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Unit { .. } => Type::Unit,
        TypeExpr::Function {
            params,
            param_modes,
            return_type,
            ..
        } => {
            let fn_params = params
                .iter()
                .zip(param_modes.iter())
                .map(|(p, mode)| {
                    let ty = resolve_type_expr_full(
                        p,
                        known_structs,
                        known_enums,
                        known_type_params,
                        known_type_aliases,
                        known_packages,
                        module_aliases,
                    );
                    FnParam { ty, mode: *mode }
                })
                .collect();
            let ret = resolve_type_expr_full(
                return_type,
                known_structs,
                known_enums,
                known_type_params,
                known_type_aliases,
                known_packages,
                module_aliases,
            );
            Type::Function {
                params: fn_params,
                return_type: Box::new(ret),
            }
        }
        TypeExpr::Union { types, .. } => {
            let resolved: Vec<Type> = types
                .iter()
                .map(|t| {
                    resolve_type_expr_full(
                        t,
                        known_structs,
                        known_enums,
                        known_type_params,
                        known_type_aliases,
                        known_packages,
                        module_aliases,
                    )
                })
                .collect();
            Type::union(resolved)
        }
    }
}

/// Builds a [`TypeIdentifier`] from a 2-segment qualified path, mapping the
/// `"std"` package label to [`Package::Std`] and everything else to
/// [`Package::Named`]. The caller has already verified that `package_label`
/// names a known package via [`path_package`] + `known_packages.contains(...)`.
fn qualified_identifier(package_label: &str, type_name: &str) -> TypeIdentifier {
    if package_label == "std" {
        TypeIdentifier::std(type_name)
    } else {
        TypeIdentifier::new(package_label, type_name)
    }
}

/// Builds a substitution map from type parameter names to concrete type arguments.
pub fn build_substitution(
    type_params: &[TypeParam],
    type_args: &[Type],
) -> std::collections::HashMap<String, Type> {
    type_params
        .iter()
        .zip(type_args.iter())
        .map(|(tp, ta)| (tp.name.clone(), ta.clone()))
        .collect()
}

/// Helper to construct a generic Named type, resolving the package via the
/// type context's name index when available. When `scope` is provided the
/// resolver prefers a same-package definition (`scope.name`) before falling
/// back to a bare/std entry.
pub fn named_generic(
    name: &str,
    type_args: Vec<Type>,
    ctx: &crate::context::TypeContext,
    scope: Option<&Package>,
) -> Type {
    let identifier = match scope {
        Some(pkg) => ctx.resolve_name_scoped(name, pkg),
        None => ctx.resolve_name(name),
    }
    .cloned()
    .unwrap_or_else(|| TypeIdentifier::unresolved(name));
    Type::Named {
        identifier,
        type_args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_generic_unresolved(name: &str, type_args: Vec<Type>) -> Type {
        Type::Named {
            identifier: TypeIdentifier::unresolved(name),
            type_args,
        }
    }

    // ---- Type::union ----

    #[test]
    fn union_empty_is_unit() {
        assert_eq!(Type::union(vec![]), Type::Unit);
    }

    #[test]
    fn union_single_collapses() {
        let ty = Type::Primitive(Primitive::I64);
        assert_eq!(Type::union(vec![ty.clone()]), ty);
    }

    #[test]
    fn union_deduplicates() {
        let a = Type::Primitive(Primitive::I64);
        let result = Type::union(vec![a.clone(), a.clone()]);
        assert_eq!(result, a);
    }

    #[test]
    fn union_sorts_by_display() {
        let a = named("Zebra");
        let b = named("Apple");
        let result = Type::union(vec![a.clone(), b.clone()]);
        assert_eq!(result, Type::Union(vec![b, a]));
    }

    #[test]
    fn union_flattens_nested() {
        let a = Type::Primitive(Primitive::I64);
        let b = Type::Primitive(Primitive::Bool);
        let c = Type::Primitive(Primitive::F64);
        let inner = Type::Union(vec![a.clone(), b.clone()]);
        let result = Type::union(vec![inner, c.clone()]);
        if let Type::Union(members) = &result {
            assert_eq!(members.len(), 3);
            assert!(members.contains(&a));
            assert!(members.contains(&b));
            assert!(members.contains(&c));
        } else {
            panic!("expected Union, got {:?}", result);
        }
    }

    // ---- Type::display ----

    #[test]
    fn display_primitives() {
        assert_eq!(Type::Primitive(Primitive::I64).display(), "Int");
        assert_eq!(Type::Primitive(Primitive::Bool).display(), "Bool");
        assert_eq!(Type::Primitive(Primitive::String).display(), "String");
        assert_eq!(Type::Primitive(Primitive::F64).display(), "Float");
    }

    #[test]
    fn display_named_types() {
        assert_eq!(named("Point").display(), "Point");
        assert_eq!(named("Color").display(), "Color");
    }

    #[test]
    fn display_generic_instance() {
        let ty = named_generic_unresolved("Option", vec![Type::Primitive(Primitive::I64)]);
        assert_eq!(ty.display(), "Option<Int>");
    }

    #[test]
    fn display_nested_generic() {
        let ty = named_generic_unresolved(
            "Result",
            vec![
                named_generic_unresolved("List", vec![Type::Primitive(Primitive::I64)]),
                Type::Primitive(Primitive::String),
            ],
        );
        assert_eq!(ty.display(), "Result<List<Int>, String>");
    }

    #[test]
    fn display_function_type() {
        let ty = Type::Function {
            params: vec![FnParam::borrow(Type::Primitive(Primitive::I64))],
            return_type: Box::new(Type::Primitive(Primitive::Bool)),
        };
        assert_eq!(ty.display(), "fn (Int) -> Bool");
    }

    #[test]
    fn display_function_type_with_move() {
        let ty = Type::Function {
            params: vec![FnParam::moved(Type::Primitive(Primitive::I64))],
            return_type: Box::new(Type::Primitive(Primitive::Bool)),
        };
        assert_eq!(ty.display(), "fn (move Int) -> Bool");
    }

    #[test]
    fn display_union() {
        let ty = Type::Union(vec![named("Cat"), named("Dog")]);
        assert_eq!(ty.display(), "Cat | Dog");
    }

    #[test]
    fn display_indirect_delegates() {
        let inner = named("Node");
        let ty = Type::Indirect(Box::new(inner));
        assert_eq!(ty.display(), "Node");
    }

    #[test]
    fn display_unit_and_unknown() {
        assert_eq!(Type::Unit.display(), "()");
        assert_eq!(Type::Unknown.display(), "unknown");
        assert_eq!(Type::Error.display(), "<error>");
    }

    // ---- Type::is_copy ----

    #[test]
    fn is_copy_numeric_primitives() {
        assert!(Type::Primitive(Primitive::I64).is_copy());
        assert!(Type::Primitive(Primitive::F64).is_copy());
        assert!(Type::Primitive(Primitive::Bool).is_copy());
        assert!(Type::Primitive(Primitive::U8).is_copy());
    }

    #[test]
    fn is_copy_string_is_move() {
        assert!(!Type::Primitive(Primitive::String).is_copy());
    }

    #[test]
    fn is_copy_struct_is_move() {
        assert!(!named("Point").is_copy());
    }

    #[test]
    fn is_copy_unit_is_copy() {
        assert!(Type::Unit.is_copy());
    }

    #[test]
    fn is_copy_function_is_copy() {
        let ty = Type::Function {
            params: vec![],
            return_type: Box::new(Type::Unit),
        };
        assert!(ty.is_copy());
    }

    #[test]
    fn is_copy_union_of_copies() {
        let ty = Type::Union(vec![
            Type::Primitive(Primitive::I64),
            Type::Primitive(Primitive::Bool),
        ]);
        assert!(ty.is_copy());
    }

    #[test]
    fn is_copy_union_with_move() {
        let ty = Type::Union(vec![Type::Primitive(Primitive::I64), named("Foo")]);
        assert!(!ty.is_copy());
    }

    // ---- Type::is_known ----

    #[test]
    fn is_known_concrete_types() {
        assert!(Type::Primitive(Primitive::I64).is_known());
        assert!(named("Foo").is_known());
        assert!(named("Color").is_known());
        assert!(Type::Unit.is_known());
    }

    #[test]
    fn is_known_unknown_and_error() {
        assert!(!Type::Unknown.is_known());
        assert!(!Type::Error.is_known());
    }

    #[test]
    fn is_known_parameter() {
        assert!(!Type::Parameter("T".into()).is_known());
    }

    #[test]
    fn is_known_indirect_delegates() {
        assert!(Type::Indirect(Box::new(named("X"))).is_known());
        assert!(!Type::Indirect(Box::new(Type::Unknown)).is_known());
    }

    #[test]
    fn is_known_union_all_known() {
        let ty = Type::Union(vec![Type::Primitive(Primitive::I64), named("Foo")]);
        assert!(ty.is_known());
    }

    #[test]
    fn is_known_union_with_unknown() {
        let ty = Type::Union(vec![Type::Primitive(Primitive::I64), Type::Unknown]);
        assert!(!ty.is_known());
    }

    // ---- Type::is_numeric ----

    #[test]
    fn is_numeric_integers_and_floats() {
        assert!(Type::Primitive(Primitive::I64).is_numeric());
        assert!(Type::Primitive(Primitive::I32).is_numeric());
        assert!(Type::Primitive(Primitive::U8).is_numeric());
        assert!(Type::Primitive(Primitive::F64).is_numeric());
        assert!(Type::Primitive(Primitive::F32).is_numeric());
    }

    #[test]
    fn is_numeric_non_numeric() {
        assert!(!Type::Primitive(Primitive::Bool).is_numeric());
        assert!(!Type::Primitive(Primitive::String).is_numeric());
        assert!(!named("Foo").is_numeric());
    }

    // ---- Primitive::display / from_name round-trip ----

    #[test]
    fn primitive_display_from_name_roundtrip() {
        let all = [
            Primitive::Binary,
            Primitive::Bits,
            Primitive::Bool,
            Primitive::F32,
            Primitive::F64,
            Primitive::I8,
            Primitive::I16,
            Primitive::I32,
            Primitive::I64,
            Primitive::String,
            Primitive::U8,
            Primitive::U16,
            Primitive::U32,
            Primitive::U64,
        ];
        for p in &all {
            let name = p.display();
            let roundtrip = Primitive::from_name(name);
            assert_eq!(roundtrip, Some(*p), "failed for {}", name);
        }
    }

    #[test]
    fn primitive_from_name_unknown() {
        assert_eq!(Primitive::from_name("Void"), None);
        assert_eq!(Primitive::from_name("int"), None);
        assert_eq!(Primitive::from_name(""), None);
    }
}
