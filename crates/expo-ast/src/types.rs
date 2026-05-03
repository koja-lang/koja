//! Resolved type representations for the Expo type system.
//!
//! These types live in `expo-ast` so that AST nodes can carry resolved type
//! information without creating a dependency on `expo-typecheck`.

use std::collections::HashMap;

use crate::ast::PassMode;
use crate::identifier::TypeIdentifier;

/// A function parameter with its resolved type and passing mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnParam {
    pub ty: Type,
    pub mode: PassMode,
}

impl FnParam {
    pub fn borrow(ty: Type) -> Self {
        Self {
            ty,
            mode: PassMode::Borrow,
        }
    }

    pub fn moved(ty: Type) -> Self {
        Self {
            ty,
            mode: PassMode::Move,
        }
    }
}

/// The resolved type representation used throughout the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// A user-defined type (struct or enum), optionally with type arguments.
    /// Point, Direction, List<Int>, Option<String>
    Named {
        identifier: TypeIdentifier,
        type_args: Vec<Type>,
    },

    /// Error recovery sentinel (type-check failed, continue checking)
    Error,

    /// A function type: fn (A, B) -> C
    Function {
        params: Vec<FnParam>,
        return_type: Box<Type>,
    },

    /// Indirection for recursive types. Transparent to the user: display,
    /// mangling, and unification all delegate to the inner type.
    Indirect(Box<Type>),

    /// A built-in primitive: Int, Float, Bool, String, Binary, Bits
    Primitive(Primitive),

    /// An unresolved type parameter: T in List<T>
    Parameter(String),

    /// A raw C pointer: CPtr<T>
    Pointer(Box<Type>),

    /// A union type: A | B | C
    Union(Vec<Type>),

    /// The unit type: ()
    Unit,

    /// Type could not be resolved
    Unknown,
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
    pub fn display(&self) -> std::string::String {
        match self {
            Type::Named {
                identifier,
                type_args,
            } => {
                if type_args.is_empty() {
                    identifier.to_string()
                } else {
                    let args: Vec<std::string::String> =
                        type_args.iter().map(|t| t.display()).collect();
                    format!("{}<{}>", identifier, args.join(", "))
                }
            }
            Type::Error => "<error>".to_string(),
            Type::Function {
                params,
                return_type,
            } => {
                let p: Vec<std::string::String> = params
                    .iter()
                    .map(|fp| {
                        if fp.mode == PassMode::Move {
                            format!("move {}", fp.ty.display())
                        } else {
                            fp.ty.display()
                        }
                    })
                    .collect();
                format!("fn ({}) -> {}", p.join(", "), return_type.display())
            }
            Type::Indirect(inner) => inner.display(),
            Type::Pointer(inner) => format!("CPtr<{}>", inner.display()),
            Type::Primitive(p) => p.display().to_string(),
            Type::Parameter(name) => name.clone(),
            Type::Union(members) => {
                let parts: Vec<std::string::String> = members.iter().map(|t| t.display()).collect();
                parts.join(" | ")
            }
            Type::Unit => "()".to_string(),
            Type::Unknown => "unknown".to_string(),
        }
    }

    /// Copy types are implicitly duplicated on assignment and never trigger
    /// use-after-move. Move types transfer ownership on assignment.
    pub fn is_copy(&self) -> bool {
        match self {
            Type::Primitive(Primitive::String) => false,
            Type::Primitive(_) => true,
            Type::Unit => true,
            Type::Function { .. } => true,
            Type::Pointer(_) => true,
            Type::Indirect(_) | Type::Named { .. } => false,
            Type::Union(members) => members.iter().all(|m| m.is_copy()),
            Type::Parameter(_) | Type::Unknown | Type::Error => true,
        }
    }

    /// Returns true if this type is a concrete, resolved type (not `Unknown`,
    /// `Error`, or `Parameter`).
    pub fn is_known(&self) -> bool {
        match self {
            Type::Unknown | Type::Error | Type::Parameter(_) => false,
            Type::Named { type_args, .. } => type_args.is_empty(),
            Type::Indirect(inner) => inner.is_known(),
            Type::Pointer(inner) => inner.is_known(),
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

/// Returns true if two types are compatible numeric types (both int or both float).
pub fn numeric_compatible(a: &Type, b: &Type) -> bool {
    if let (Type::Primitive(pa), Type::Primitive(pb)) = (a, b) {
        (pa.is_integer() && pb.is_integer()) || (pa.is_float() && pb.is_float())
    } else {
        false
    }
}

/// Attempts to unify a parameter type (possibly containing [`Type::Parameter`]s) with a
/// concrete argument type. Binds type variables in `subst` on first encounter, and
/// checks consistency on subsequent encounters. Returns `false` if the types conflict.
pub fn unify(param_ty: &Type, arg_ty: &Type, subst: &mut HashMap<String, Type>) -> bool {
    match (param_ty, arg_ty) {
        (Type::Indirect(inner), other) | (other, Type::Indirect(inner)) => {
            unify(inner, other, subst)
        }
        (Type::Parameter(name), _) => {
            if let Some(existing) = subst.get(name) {
                existing == arg_ty || numeric_compatible(existing, arg_ty)
            } else {
                subst.insert(name.clone(), arg_ty.clone());
                true
            }
        }
        (
            Type::Named {
                identifier: a,
                type_args: aa,
            },
            Type::Named {
                identifier: b,
                type_args: ba,
            },
        ) => {
            if a.name != b.name {
                return false;
            }
            if aa.is_empty() || ba.is_empty() {
                return true;
            }
            if aa.len() != ba.len() {
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
                if a.mode != b.mode || !unify(&a.ty, &b.ty, subst) {
                    return false;
                }
            }
            unify(ra, rb, subst)
        }
        (Type::Pointer(a), Type::Pointer(b)) => unify(a, b, subst),
        (Type::Union(a), Type::Union(b)) => a == b,
        (Type::Unit, Type::Unit) => true,
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        _ => false,
    }
}

/// Replaces all [`Type::Parameter`]s in `ty` with their concrete bindings from `subst`.
pub fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Parameter(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|fp| FnParam {
                    ty: substitute(&fp.ty, subst),
                    mode: fp.mode,
                })
                .collect(),
            return_type: Box::new(substitute(return_type, subst)),
        },
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let substituted: Vec<Type> = type_args.iter().map(|t| substitute(t, subst)).collect();
            if substituted.iter().any(contains_parameter) {
                Type::Named {
                    identifier: identifier.clone(),
                    type_args: substituted,
                }
            } else {
                // Flatten to a non-generic Named whose `name` field holds the
                // full mangled key. Package is set to `Unresolved` so
                // `qualified_name()` just returns the already-qualified mangled
                // string directly (avoiding a double-package prefix).
                let mangled = mangle_name(identifier, &substituted);
                Type::Named {
                    identifier: TypeIdentifier::unresolved_owned(mangled),
                    type_args: vec![],
                }
            }
        }
        Type::Indirect(inner) => Type::Indirect(Box::new(substitute(inner, subst))),
        Type::Pointer(inner) => Type::Pointer(Box::new(substitute(inner, subst))),
        Type::Union(members) => Type::union(members.iter().map(|m| substitute(m, subst)).collect()),
        _ => ty.clone(),
    }
}

/// Like [`substitute`], but preserves [`Type::Named`] with type args instead of
/// collapsing fully-resolved instances to mangled names.
pub fn substitute_preserving(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Parameter(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|fp| FnParam {
                    ty: substitute_preserving(&fp.ty, subst),
                    mode: fp.mode,
                })
                .collect(),
            return_type: Box::new(substitute_preserving(return_type, subst)),
        },
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Type::Named {
            identifier: identifier.clone(),
            type_args: type_args
                .iter()
                .map(|t| substitute_preserving(t, subst))
                .collect(),
        },
        Type::Indirect(inner) => Type::Indirect(Box::new(substitute_preserving(inner, subst))),
        Type::Pointer(inner) => Type::Pointer(Box::new(substitute_preserving(inner, subst))),
        Type::Union(members) => Type::union(
            members
                .iter()
                .map(|m| substitute_preserving(m, subst))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

/// Produces a mangled symbol name for a type.
///
/// The base component follows the same convention as
/// `expo_ir::lower::naming::method_symbol_prefix`: stdlib and unresolved-package types
/// use their bare name (so `Global.String` → `String`), while user packages
/// stay fully qualified (`alpha.Config` → `alpha.Config`). This keeps
/// mangled names consistent with how function symbols are registered
/// (`String_length`, `alpha.Config_new`, etc.) and prevents cross-package
/// collisions for user types without changing stdlib symbol names.
///
/// For generic instances the base is followed by `_$...$` containing the
/// mangled type arguments: `Global.List<Int>` → `List_$Int$`,
/// `alpha.Pair<Int, String>` → `alpha.Pair_$Int.String$`, and
/// `Global.List<Global.Pair<Int, Int>>` → `List_$Pair_$Int.Int$$`.
pub fn mangle_name(
    id: &crate::identifier::TypeIdentifier,
    type_args: &[Type],
) -> std::string::String {
    let base = mangle_base(id);
    if type_args.is_empty() {
        return base;
    }
    let args: Vec<std::string::String> = type_args.iter().map(mangle_type).collect();
    format!("{}_${}$", base, args.join("."))
}

/// Returns the symbol-prefix component of a [`TypeIdentifier`]. Matches
/// `expo_ir::lower::naming::method_symbol_prefix`: bare name for stdlib/unresolved,
/// `{package}.{name}` for user packages.
fn mangle_base(id: &crate::identifier::TypeIdentifier) -> std::string::String {
    match &id.package {
        crate::identifier::Package::Named(pkg) => format!("{pkg}.{}", id.name),
        crate::identifier::Package::Global | crate::identifier::Package::Unresolved => {
            id.name.clone()
        }
    }
}

/// Produces a mangled string for a [`Type`]. Named types use the same
/// symbol-prefix convention as [`mangle_name`] so nested generic
/// arguments from different user packages do not alias
/// (`List_$alpha.Config$` vs `List_$beta.Config$`) while stdlib types
/// continue to use their bare names (`List_$Int$`, `Option_$String$`).
pub fn mangle_type(ty: &Type) -> std::string::String {
    match ty {
        Type::Indirect(inner) => mangle_type(inner),
        Type::Primitive(p) => p.display().to_string(),
        Type::Named {
            identifier,
            type_args,
        } => mangle_name(identifier, type_args),
        Type::Pointer(inner) => format!("CPtr_${}$", mangle_type(inner)),
        Type::Parameter(n) => n.clone(),
        Type::Unit => "unit".to_string(),
        Type::Function {
            params,
            return_type,
        } => {
            let p: Vec<std::string::String> = params
                .iter()
                .map(|fp| {
                    let t = mangle_type(&fp.ty);
                    if fp.mode == PassMode::Move {
                        format!("move_{t}")
                    } else {
                        t
                    }
                })
                .collect();
            format!("fn_{}__{}", p.join("_"), mangle_type(return_type))
        }
        Type::Union(members) => {
            let parts: Vec<std::string::String> = members.iter().map(mangle_type).collect();
            format!("Union_${}$", parts.join("."))
        }
        _ => "unknown".to_string(),
    }
}

/// Mangles type arguments into a suffix for a method name. For example,
/// `foo<Int>` becomes `foo_$Int$`. Used when building function symbols for
/// generic method calls where the base is not a type but a method name.
pub fn mangle_method_suffix(method: &str, type_args: &[Type]) -> std::string::String {
    if type_args.is_empty() {
        return method.to_string();
    }
    let args: Vec<std::string::String> = type_args.iter().map(mangle_type).collect();
    format!("{}_${}$", method, args.join("."))
}

/// Returns true if the type or any nested type contains a [`Type::Parameter`].
pub fn contains_parameter(ty: &Type) -> bool {
    match ty {
        Type::Parameter(_) => true,
        Type::Function {
            params,
            return_type,
        } => params.iter().any(|fp| contains_parameter(&fp.ty)) || contains_parameter(return_type),
        Type::Named { type_args, .. } => type_args.iter().any(contains_parameter),
        Type::Indirect(inner) => contains_parameter(inner),
        Type::Pointer(inner) => contains_parameter(inner),
        Type::Union(members) => members.iter().any(contains_parameter),
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
    Type::Named {
        identifier: TypeIdentifier::global("Pair"),
        type_args: vec![
            m.clone(),
            Type::Named {
                identifier: TypeIdentifier::global("Option"),
                type_args: vec![Type::Named {
                    identifier: TypeIdentifier::global("ReplyTo"),
                    type_args: vec![r.clone()],
                }],
            },
        ],
    }
}

/// Helper to construct a non-generic Named type with an unresolved package.
pub fn named(name: &str) -> Type {
    Type::Named {
        identifier: TypeIdentifier::unresolved(name),
        type_args: vec![],
    }
}

/// Extracts the `TypeIdentifier` from a `Type::Named`, if present.
pub fn type_identifier(ty: &Type) -> Option<&TypeIdentifier> {
    match ty {
        Type::Named { identifier, .. } => Some(identifier),
        _ => None,
    }
}

/// Constructs a non-generic Named type with `Package::Global`. Use for known
/// stdlib types (e.g. `IOReady`, `Lifecycle`) where the package is certain.
pub fn named_global(name: &str) -> Type {
    Type::Named {
        identifier: TypeIdentifier::global(name),
        type_args: vec![],
    }
}

/// Constructs a generic Named type with `Package::Global`. Use for known
/// stdlib generic types (e.g. `Option<T>`, `List<T>`) where the package is
/// certain and no `TypeContext` resolution is needed.
pub fn named_generic_global(name: &str, type_args: Vec<Type>) -> Type {
    Type::Named {
        identifier: TypeIdentifier::global(name),
        type_args,
    }
}
