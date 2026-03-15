use std::collections::HashMap;

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
    Primitive(Primitive),
    Struct(String),
    Tuple(Vec<Type>),
    TypeVar(String),
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
                format!("({}) -> {}", p.join(", "), return_type.display())
            }
            Type::GenericInstance {
                base, type_args, ..
            } => {
                let args: Vec<String> = type_args.iter().map(|t| t.display()).collect();
                format!("{}<{}>", base, args.join(", "))
            }
            Type::Primitive(p) => p.display().to_string(),
            Type::Struct(name) => name.clone(),
            Type::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(|t| t.display()).collect();
                format!("({})", inner.join(", "))
            }
            Type::TypeVar(name) => name.clone(),
            Type::Unit => "()".to_string(),
            Type::Unknown => "unknown".to_string(),
        }
    }

    /// Returns true if this type is a concrete, resolved type (not `Unknown`, `Error`, or `TypeVar`).
    pub fn is_known(&self) -> bool {
        !matches!(
            self,
            Type::Unknown | Type::Error | Type::TypeVar(_) | Type::GenericInstance { .. }
        )
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
            Primitive::Bool => "bool",
            Primitive::F32 => "f32",
            Primitive::F64 => "f64",
            Primitive::I8 => "i8",
            Primitive::I16 => "i16",
            Primitive::I32 => "i32",
            Primitive::I64 => "i64",
            Primitive::String => "string",
            Primitive::U8 => "u8",
            Primitive::U16 => "u16",
            Primitive::U32 => "u32",
            Primitive::U64 => "u64",
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
            "bool" => Some(Primitive::Bool),
            "f32" => Some(Primitive::F32),
            "f64" => Some(Primitive::F64),
            "i8" => Some(Primitive::I8),
            "i16" => Some(Primitive::I16),
            "i32" => Some(Primitive::I32),
            "i64" => Some(Primitive::I64),
            "string" => Some(Primitive::String),
            "u8" => Some(Primitive::U8),
            "u16" => Some(Primitive::U16),
            "u32" => Some(Primitive::U32),
            "u64" => Some(Primitive::U64),
            _ => None,
        }
    }
}

/// Converts an AST type expression into a resolved [`Type`], looking up user-defined
/// struct and enum names from the provided slices.
pub fn resolve_type_expr(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
) -> Type {
    resolve_type_expr_with_params(type_expr, known_structs, known_enums, &[])
}

/// Like [`resolve_type_expr`] but also resolves type parameter names (e.g. `T`, `A`)
/// to [`Type::TypeVar`] when they appear in generic function/struct definitions.
pub fn resolve_type_expr_with_params(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_params: &[&str],
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
        TypeExpr::Ref { .. } => Type::Unknown,
        TypeExpr::Named { path, .. } => {
            if path.len() == 1 {
                let name = path[0].as_str();
                if known_type_params.contains(&name) {
                    return Type::TypeVar(name.to_string());
                }
                match name {
                    "string" => Type::Primitive(Primitive::String),
                    "bool" => Type::Primitive(Primitive::Bool),
                    "f32" => Type::Primitive(Primitive::F32),
                    "f64" => Type::Primitive(Primitive::F64),
                    "i8" => Type::Primitive(Primitive::I8),
                    "i16" => Type::Primitive(Primitive::I16),
                    "i32" => Type::Primitive(Primitive::I32),
                    "i64" => Type::Primitive(Primitive::I64),
                    "u8" => Type::Primitive(Primitive::U8),
                    "u16" => Type::Primitive(Primitive::U16),
                    "u32" => Type::Primitive(Primitive::U32),
                    "u64" => Type::Primitive(Primitive::U64),
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
        TypeExpr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements
                .iter()
                .map(|e| {
                    resolve_type_expr_with_params(e, known_structs, known_enums, known_type_params)
                })
                .collect();
            Type::Tuple(types)
        }
        TypeExpr::Unit { .. } => Type::Unit,
    }
}

/// Attempts to unify a parameter type (possibly containing [`Type::TypeVar`]s) with a
/// concrete argument type. Binds type variables in `subst` on first encounter, and
/// checks consistency on subsequent encounters. Returns `false` if the types conflict.
pub fn unify(param_ty: &Type, arg_ty: &Type, subst: &mut HashMap<String, Type>) -> bool {
    match (param_ty, arg_ty) {
        (Type::TypeVar(name), _) => {
            if let Some(existing) = subst.get(name) {
                existing == arg_ty
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
        (Type::Primitive(a), Type::Primitive(b)) => a == b,
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
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| substitute(e, subst)).collect()),
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

fn mangle_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => p.display().to_string(),
        Type::Struct(n) | Type::Enum(n) => n.clone(),
        Type::TypeVar(n) => n.clone(),
        Type::Unit => "unit".to_string(),
        Type::GenericInstance {
            base, type_args, ..
        } => mangle_name(base, type_args),
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
        Type::Tuple(elems) => elems.iter().any(contains_type_var),
        _ => false,
    }
}
