use std::collections::{BTreeMap, HashMap};

use expo_ast::ast::TypeExpr;

/// The resolved type representation used throughout the type checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Enum(String),
    Error,
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },
    GenericInstance {
        base: String,
        kind: GenericKind,
        type_args: Vec<Type>,
    },
    /// A heap-allocated indirection inserted by cycle detection for recursive
    /// types. Transparent to the user: display, mangling, and unification all
    /// delegate to the inner type.
    Indirect(Box<Type>),
    Primitive(Primitive),
    Struct(String),
    TypeVar(String),
    Union(Vec<Type>),
    Unit,
    Unknown,
}

/// Whether a generic instance refers to a struct or enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericKind {
    Struct,
    Enum,
}

/// Built-in primitive types with known sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    Binary,
    Bits,
    Bool,
    F32,
    F64,
    I8,
    I16,
    I32,
    I64,
    String,
    U8,
    U16,
    U32,
    U64,
}

impl Type {
    /// Constructs a canonical union type: sorts, deduplicates, flattens nested
    /// unions, and collapses single-element unions to the inner type.
    pub fn union(types: Vec<Type>) -> Type {
        let mut flat = Vec::new();
        for ty in types {
            match ty {
                Type::Union(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        flat.sort_by_key(|a| a.display());
        flat.dedup();
        match flat.len() {
            0 => Type::Unit,
            1 => flat.into_iter().next().unwrap(),
            _ => Type::Union(flat),
        }
    }

    /// Returns a human-readable string representation of this type for diagnostics.
    pub fn display(&self) -> String {
        match self {
            Type::Enum(name) => name.clone(),
            Type::Error => "<error>".to_string(),
            Type::Function {
                params,
                return_type,
            } => {
                let p: Vec<String> = params.iter().map(|t| t.display()).collect();
                format!("fn({}) -> {}", p.join(", "), return_type.display())
            }
            Type::GenericInstance {
                base, type_args, ..
            } => {
                let args: Vec<String> = type_args.iter().map(|t| t.display()).collect();
                format!("{}<{}>", base, args.join(", "))
            }
            Type::Indirect(inner) => inner.display(),
            Type::Primitive(p) => p.display().to_string(),
            Type::Struct(name) => name.clone(),
            Type::TypeVar(name) => name.clone(),
            Type::Union(members) => {
                let parts: Vec<String> = members.iter().map(|t| t.display()).collect();
                parts.join(" | ")
            }
            Type::Unit => "()".to_string(),
            Type::Unknown => "unknown".to_string(),
        }
    }

    /// Copy types are implicitly duplicated on assignment and never trigger
    /// use-after-move. Move types transfer ownership on assignment.
    ///
    /// Copy: all numeric primitives, Bool, Unit, function pointers.
    /// Move: String, structs, enums (including generic instances like Option<T>).
    pub fn is_copy(&self) -> bool {
        match self {
            Type::Primitive(Primitive::String) => false,
            Type::Primitive(_) => true,
            Type::Unit => true,
            Type::Function { .. } => true,
            Type::Indirect(_) | Type::Struct(_) | Type::Enum(_) | Type::GenericInstance { .. } => {
                false
            }
            Type::Union(members) => members.iter().all(|m| m.is_copy()),
            Type::TypeVar(_) | Type::Unknown | Type::Error => true,
        }
    }

    /// Returns true if this type is a concrete, resolved type (not `Unknown`, `Error`, or `TypeVar`).
    pub fn is_known(&self) -> bool {
        match self {
            Type::Unknown | Type::Error | Type::TypeVar(_) | Type::GenericInstance { .. } => false,
            Type::Indirect(inner) => inner.is_known(),
            Type::Union(members) => members.iter().all(|m| m.is_known()),
            _ => true,
        }
    }

    /// Returns true if this type is an integer or floating-point primitive.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Primitive(
                Primitive::F32
                    | Primitive::F64
                    | Primitive::I8
                    | Primitive::I16
                    | Primitive::I32
                    | Primitive::I64
                    | Primitive::U8
                    | Primitive::U16
                    | Primitive::U32
                    | Primitive::U64
            )
        )
    }
}

impl Primitive {
    /// Returns the Expo source-level name of this primitive type.
    pub fn display(&self) -> &'static str {
        match self {
            Primitive::Binary => "Binary",
            Primitive::Bits => "Bits",
            Primitive::Bool => "Bool",
            Primitive::F32 => "Float32",
            Primitive::F64 => "Float",
            Primitive::I8 => "Int8",
            Primitive::I16 => "Int16",
            Primitive::I32 => "Int32",
            Primitive::I64 => "Int",
            Primitive::String => "String",
            Primitive::U8 => "UInt8",
            Primitive::U16 => "UInt16",
            Primitive::U32 => "UInt32",
            Primitive::U64 => "UInt64",
        }
    }

    /// Returns the fixed bit width of this primitive, or `None` for
    /// variable-size types (`String`, `Binary`, `Bits`).
    pub fn bit_width(&self) -> Option<u64> {
        match self {
            Primitive::Bool => Some(1),
            Primitive::I8 | Primitive::U8 => Some(8),
            Primitive::I16 | Primitive::U16 => Some(16),
            Primitive::I32 | Primitive::U32 | Primitive::F32 => Some(32),
            Primitive::I64 | Primitive::U64 | Primitive::F64 => Some(64),
            Primitive::String | Primitive::Binary | Primitive::Bits => None,
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Primitive::F32 | Primitive::F64)
    }

    /// Parses a primitive type name string back into a [`Primitive`].
    pub fn from_name(s: &str) -> Option<Primitive> {
        match s {
            "Binary" => Some(Primitive::Binary),
            "Bits" => Some(Primitive::Bits),
            "Bool" => Some(Primitive::Bool),
            "Float32" => Some(Primitive::F32),
            "Float" => Some(Primitive::F64),
            "Int8" => Some(Primitive::I8),
            "Int16" => Some(Primitive::I16),
            "Int32" => Some(Primitive::I32),
            "Int" => Some(Primitive::I64),
            "String" => Some(Primitive::String),
            "UInt8" => Some(Primitive::U8),
            "UInt16" => Some(Primitive::U16),
            "UInt32" => Some(Primitive::U32),
            "UInt64" => Some(Primitive::U64),
            _ => None,
        }
    }
}

/// Converts an AST type expression into a resolved [`Type`], looking up user-defined
/// struct and enum names from the provided slices. Pass an empty map for
/// `known_type_aliases` when none are available.
pub fn resolve_type_expr(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
) -> Type {
    resolve_type_expr_with_params(type_expr, known_structs, known_enums, &[], &BTreeMap::new())
}

/// Like [`resolve_type_expr`] but also resolves type parameter names (e.g. `T`, `A`)
/// to [`Type::TypeVar`] when they appear in generic function/struct definitions,
/// and named type aliases from the provided map.
pub fn resolve_type_expr_with_params(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
) -> Type {
    match type_expr {
        TypeExpr::Generic { path, args, .. } => {
            if path.len() == 1
                && (known_structs.contains(&path[0].as_str())
                    || known_enums.contains(&path[0].as_str()))
            {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| {
                        resolve_type_expr_with_params(
                            a,
                            known_structs,
                            known_enums,
                            known_type_params,
                            known_type_aliases,
                        )
                    })
                    .collect();
                let kind = if known_structs.contains(&path[0].as_str()) {
                    GenericKind::Struct
                } else {
                    GenericKind::Enum
                };
                Type::GenericInstance {
                    base: path[0].clone(),
                    kind,
                    type_args: resolved_args,
                }
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Named { path, .. } => {
            if path.len() == 1 {
                let name = path[0].as_str();
                if known_type_params.contains(&name) {
                    return Type::TypeVar(name.to_string());
                }
                if let Some(aliased) = known_type_aliases.get(name) {
                    return aliased.clone();
                }
                match name {
                    "Binary" => Type::Primitive(Primitive::Binary),
                    "Bits" => Type::Primitive(Primitive::Bits),
                    "String" => Type::Primitive(Primitive::String),
                    "Bool" => Type::Primitive(Primitive::Bool),
                    "Float32" => Type::Primitive(Primitive::F32),
                    "Float" => Type::Primitive(Primitive::F64),
                    "Int8" => Type::Primitive(Primitive::I8),
                    "Int16" => Type::Primitive(Primitive::I16),
                    "Int32" => Type::Primitive(Primitive::I32),
                    "Int" => Type::Primitive(Primitive::I64),
                    "UInt8" => Type::Primitive(Primitive::U8),
                    "UInt16" => Type::Primitive(Primitive::U16),
                    "UInt32" => Type::Primitive(Primitive::U32),
                    "UInt64" => Type::Primitive(Primitive::U64),
                    name => {
                        if known_structs.contains(&name) {
                            Type::Struct(name.to_string())
                        } else if known_enums.contains(&name) {
                            Type::Enum(name.to_string())
                        } else {
                            Type::Unknown
                        }
                    }
                }
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Self_ { .. } => {
            if known_type_params.contains(&"Self") {
                Type::TypeVar("Self".to_string())
            } else {
                Type::Unknown
            }
        }
        TypeExpr::Unit { .. } => Type::Unit,
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let param_types = params
                .iter()
                .map(|p| {
                    resolve_type_expr_with_params(
                        p,
                        known_structs,
                        known_enums,
                        known_type_params,
                        known_type_aliases,
                    )
                })
                .collect();
            let ret = resolve_type_expr_with_params(
                return_type,
                known_structs,
                known_enums,
                known_type_params,
                known_type_aliases,
            );
            Type::Function {
                params: param_types,
                return_type: Box::new(ret),
            }
        }
        TypeExpr::Union { types, .. } => {
            let resolved: Vec<Type> = types
                .iter()
                .map(|t| {
                    resolve_type_expr_with_params(
                        t,
                        known_structs,
                        known_enums,
                        known_type_params,
                        known_type_aliases,
                    )
                })
                .collect();
            Type::union(resolved)
        }
    }
}

/// Returns true if two types are compatible numeric types (both int or both float).
pub fn numeric_compatible(a: &Type, b: &Type) -> bool {
    if let (Type::Primitive(pa), Type::Primitive(pb)) = (a, b) {
        (pa.is_integer() && pb.is_integer()) || (pa.is_float() && pb.is_float())
    } else {
        false
    }
}

/// Attempts to unify a parameter type (possibly containing [`Type::TypeVar`]s) with a
/// concrete argument type. Binds type variables in `subst` on first encounter, and
/// checks consistency on subsequent encounters. Returns `false` if the types conflict.
pub fn unify(param_ty: &Type, arg_ty: &Type, subst: &mut HashMap<String, Type>) -> bool {
    match (param_ty, arg_ty) {
        (Type::Indirect(inner), other) | (other, Type::Indirect(inner)) => {
            unify(inner, other, subst)
        }
        (Type::TypeVar(name), _) => {
            if let Some(existing) = subst.get(name) {
                existing == arg_ty || numeric_compatible(existing, arg_ty)
            } else {
                subst.insert(name.clone(), arg_ty.clone());
                true
            }
        }
        (Type::Struct(a), Type::Struct(b)) => a == b,
        (Type::Enum(a), Type::Enum(b)) => a == b,
        (
            Type::GenericInstance {
                base: a,
                type_args: aa,
                ..
            },
            Type::GenericInstance {
                base: b,
                type_args: ba,
                ..
            },
        ) => {
            if a != b || aa.len() != ba.len() {
                return false;
            }
            for (x, y) in aa.iter().zip(ba.iter()) {
                if !unify(x, y, subst) {
                    return false;
                }
            }
            true
        }
        (Type::Primitive(a), Type::Primitive(b)) => {
            a == b || (a.is_integer() && b.is_integer()) || (a.is_float() && b.is_float())
        }
        (
            Type::Function {
                params: pa,
                return_type: ra,
            },
            Type::Function {
                params: pb,
                return_type: rb,
            },
        ) => {
            if pa.len() != pb.len() {
                return false;
            }
            for (a, b) in pa.iter().zip(pb.iter()) {
                if !unify(a, b, subst) {
                    return false;
                }
            }
            unify(ra, rb, subst)
        }
        (Type::GenericInstance { base, .. }, Type::Enum(name))
        | (Type::Enum(name), Type::GenericInstance { base, .. })
        | (Type::GenericInstance { base, .. }, Type::Struct(name))
        | (Type::Struct(name), Type::GenericInstance { base, .. }) => base == name,
        (Type::Union(a), Type::Union(b)) => a == b,
        (Type::Unit, Type::Unit) => true,
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        _ => false,
    }
}

/// Replaces all [`Type::TypeVar`]s in `ty` with their concrete bindings from `subst`.
pub fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::TypeVar(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params.iter().map(|p| substitute(p, subst)).collect(),
            return_type: Box::new(substitute(return_type, subst)),
        },
        Type::GenericInstance {
            base,
            kind,
            type_args,
        } => {
            let substituted: Vec<Type> = type_args.iter().map(|t| substitute(t, subst)).collect();
            if substituted.iter().any(contains_type_var) {
                Type::GenericInstance {
                    base: base.clone(),
                    kind: kind.clone(),
                    type_args: substituted,
                }
            } else {
                let mangled = mangle_name(base, &substituted);
                match kind {
                    GenericKind::Struct => Type::Struct(mangled),
                    GenericKind::Enum => Type::Enum(mangled),
                }
            }
        }
        Type::Indirect(inner) => Type::Indirect(Box::new(substitute(inner, subst))),
        Type::Union(members) => Type::union(members.iter().map(|m| substitute(m, subst)).collect()),
        _ => ty.clone(),
    }
}

/// Like [`substitute`], but preserves [`Type::GenericInstance`] instead of
/// collapsing fully-resolved instances to mangled `Struct`/`Enum` names.
/// Used by `resolve_type_expr` so downstream code can inspect the structured
/// generic form without re-parsing mangled names.
pub fn substitute_preserving(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::TypeVar(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|p| substitute_preserving(p, subst))
                .collect(),
            return_type: Box::new(substitute_preserving(return_type, subst)),
        },
        Type::GenericInstance {
            base,
            kind,
            type_args,
        } => Type::GenericInstance {
            base: base.clone(),
            kind: kind.clone(),
            type_args: type_args
                .iter()
                .map(|t| substitute_preserving(t, subst))
                .collect(),
        },
        Type::Indirect(inner) => Type::Indirect(Box::new(substitute_preserving(inner, subst))),
        Type::Union(members) => Type::union(
            members
                .iter()
                .map(|m| substitute_preserving(m, subst))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

/// Produces a mangled name for a monomorphized generic using a nesting-safe
/// scheme: `Pair<i32, string>` becomes `Pair_$i32.string$` and
/// `List<Pair<i32, i32>>` becomes `List_$Pair_$i32.i32$$`.
pub fn mangle_name(base: &str, type_args: &[Type]) -> String {
    if type_args.is_empty() {
        return base.to_string();
    }
    let args: Vec<String> = type_args.iter().map(mangle_type).collect();
    format!("{}_${}$", base, args.join("."))
}

pub fn mangle_type(ty: &Type) -> String {
    match ty {
        Type::Indirect(inner) => mangle_type(inner),
        Type::Primitive(p) => p.display().to_string(),
        Type::Struct(n) | Type::Enum(n) => n.clone(),
        Type::TypeVar(n) => n.clone(),
        Type::Unit => "unit".to_string(),
        Type::GenericInstance {
            base, type_args, ..
        } => mangle_name(base, type_args),
        Type::Function {
            params,
            return_type,
        } => {
            let p: Vec<String> = params.iter().map(mangle_type).collect();
            format!("fn_{}__{}", p.join("_"), mangle_type(return_type))
        }
        Type::Union(members) => {
            let parts: Vec<String> = members.iter().map(mangle_type).collect();
            format!("Union_${}$", parts.join("."))
        }
        _ => "unknown".to_string(),
    }
}

/// Builds a substitution map from type parameter names to concrete type arguments.
pub fn build_substitution(type_params: &[String], type_args: &[Type]) -> HashMap<String, Type> {
    type_params
        .iter()
        .zip(type_args.iter())
        .map(|(tp, ta)| (tp.clone(), ta.clone()))
        .collect()
}

/// Returns true if the type or any nested type contains a [`Type::TypeVar`].
pub fn contains_type_var(ty: &Type) -> bool {
    match ty {
        Type::TypeVar(_) => true,
        Type::Function {
            params,
            return_type,
        } => params.iter().any(contains_type_var) || contains_type_var(return_type),
        Type::GenericInstance { type_args, .. } => type_args.iter().any(contains_type_var),
        Type::Indirect(inner) => contains_type_var(inner),
        Type::Union(members) => members.iter().any(contains_type_var),
        _ => false,
    }
}

/// Returns the inner type if `ty` is `Indirect`, otherwise returns `ty` itself.
pub fn unwrap_indirect(ty: &Type) -> &Type {
    match ty {
        Type::Indirect(inner) => inner,
        other => other,
    }
}

/// Builds the mailbox envelope type `Pair<M, Option<ReplyTo<R>>>` from M and R.
pub fn process_envelope_type(m: &Type, r: &Type) -> Type {
    Type::GenericInstance {
        base: "Pair".to_string(),
        kind: GenericKind::Struct,
        type_args: vec![
            m.clone(),
            Type::GenericInstance {
                base: "Option".to_string(),
                kind: GenericKind::Enum,
                type_args: vec![Type::GenericInstance {
                    base: "ReplyTo".to_string(),
                    kind: GenericKind::Struct,
                    type_args: vec![r.clone()],
                }],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let a = Type::Struct("Zebra".into());
        let b = Type::Struct("Apple".into());
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
    fn display_struct_and_enum() {
        assert_eq!(Type::Struct("Point".into()).display(), "Point");
        assert_eq!(Type::Enum("Color".into()).display(), "Color");
    }

    #[test]
    fn display_generic_instance() {
        let ty = Type::GenericInstance {
            base: "Option".into(),
            kind: GenericKind::Enum,
            type_args: vec![Type::Primitive(Primitive::I64)],
        };
        assert_eq!(ty.display(), "Option<Int>");
    }

    #[test]
    fn display_nested_generic() {
        let ty = Type::GenericInstance {
            base: "Result".into(),
            kind: GenericKind::Enum,
            type_args: vec![
                Type::GenericInstance {
                    base: "List".into(),
                    kind: GenericKind::Struct,
                    type_args: vec![Type::Primitive(Primitive::I64)],
                },
                Type::Primitive(Primitive::String),
            ],
        };
        assert_eq!(ty.display(), "Result<List<Int>, String>");
    }

    #[test]
    fn display_function_type() {
        let ty = Type::Function {
            params: vec![Type::Primitive(Primitive::I64)],
            return_type: Box::new(Type::Primitive(Primitive::Bool)),
        };
        assert_eq!(ty.display(), "fn(Int) -> Bool");
    }

    #[test]
    fn display_union() {
        let ty = Type::Union(vec![Type::Struct("Cat".into()), Type::Struct("Dog".into())]);
        assert_eq!(ty.display(), "Cat | Dog");
    }

    #[test]
    fn display_indirect_delegates() {
        let inner = Type::Struct("Node".into());
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
        assert!(!Type::Struct("Point".into()).is_copy());
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
        let ty = Type::Union(vec![
            Type::Primitive(Primitive::I64),
            Type::Struct("Foo".into()),
        ]);
        assert!(!ty.is_copy());
    }

    // ---- Type::is_known ----

    #[test]
    fn is_known_concrete_types() {
        assert!(Type::Primitive(Primitive::I64).is_known());
        assert!(Type::Struct("Foo".into()).is_known());
        assert!(Type::Enum("Color".into()).is_known());
        assert!(Type::Unit.is_known());
    }

    #[test]
    fn is_known_unknown_and_error() {
        assert!(!Type::Unknown.is_known());
        assert!(!Type::Error.is_known());
    }

    #[test]
    fn is_known_type_var() {
        assert!(!Type::TypeVar("T".into()).is_known());
    }

    #[test]
    fn is_known_indirect_delegates() {
        assert!(Type::Indirect(Box::new(Type::Struct("X".into()))).is_known());
        assert!(!Type::Indirect(Box::new(Type::Unknown)).is_known());
    }

    #[test]
    fn is_known_union_all_known() {
        let ty = Type::Union(vec![
            Type::Primitive(Primitive::I64),
            Type::Struct("Foo".into()),
        ]);
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
        assert!(!Type::Struct("Foo".into()).is_numeric());
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
