use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use expo_ast::ast::{PassMode, TypeExpr, TypeParam};

use crate::context::FnParam;

/// Which package a type belongs to. Used by [`TypeIdentifier`] to distinguish
/// types with the same name from different packages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Package {
    /// The built-in standard library (auto-imported).
    Std,
    /// A named package (e.g. `json`, `net`, or the user's project name).
    Named(String),
    /// Package not yet determined. Present only during early pipeline stages;
    /// resolved to a concrete package before codegen.
    Unresolved,
}

impl fmt::Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Package::Std => write!(f, "std"),
            Package::Named(name) => write!(f, "{name}"),
            Package::Unresolved => Ok(()),
        }
    }
}

/// A canonical, package-qualified identifier for a user-defined type.
/// Every struct, enum, and protocol carries one of these throughout the
/// compiler pipeline, ensuring types from different packages never collide.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeIdentifier {
    pub package: Package,
    pub name: String,
}

impl TypeIdentifier {
    /// Creates a TypeIdentifier for a type in the `std` package.
    pub fn std(name: &str) -> Self {
        Self {
            package: Package::Std,
            name: name.to_string(),
        }
    }

    /// Creates a TypeIdentifier with an explicit named package.
    pub fn new(package: &str, name: &str) -> Self {
        Self {
            package: Package::Named(package.to_string()),
            name: name.to_string(),
        }
    }

    /// Creates a TypeIdentifier with an unresolved package. All call sites
    /// will be updated in Phase 3 to use real packages.
    pub fn unresolved(name: &str) -> Self {
        Self {
            package: Package::Unresolved,
            name: name.to_string(),
        }
    }

    /// Same as [`Self::unresolved`] but takes an owned String to avoid cloning.
    pub fn unresolved_owned(name: String) -> Self {
        Self {
            package: Package::Unresolved,
            name,
        }
    }

    pub fn is_std(&self) -> bool {
        self.package == Package::Std
    }

    /// Returns a mangled name suitable for LLVM symbols.
    /// Std and unresolved packages use the bare name; named packages
    /// use `pkg_Name` to guarantee uniqueness.
    pub fn mangled(&self) -> String {
        match &self.package {
            Package::Std | Package::Unresolved => self.name.clone(),
            Package::Named(pkg) => format!("{pkg}_{}", self.name),
        }
    }
}

impl fmt::Display for TypeIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.package {
            Package::Std | Package::Unresolved => write!(f, "{}", self.name),
            Package::Named(pkg) => write!(f, "{pkg}.{}", self.name),
        }
    }
}

/// The resolved type representation used throughout the type checker.
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
        params: Vec<crate::context::FnParam>,
        return_type: Box<Type>,
    },

    /// Indirection for recursive types. Transparent to the user: display,
    /// mangling, and unification all delegate to the inner type.
    Indirect(Box<Type>),

    /// A built-in primitive: Int, Float, Bool, String, Binary, Bits
    Primitive(Primitive),

    /// An unresolved type parameter: T in List<T>
    Parameter(String),

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
    pub fn display(&self) -> String {
        match self {
            Type::Named {
                identifier,
                type_args,
            } => {
                if type_args.is_empty() {
                    identifier.to_string()
                } else {
                    let args: Vec<String> = type_args.iter().map(|t| t.display()).collect();
                    format!("{}<{}>", identifier, args.join(", "))
                }
            }
            Type::Error => "<error>".to_string(),
            Type::Function {
                params,
                return_type,
            } => {
                let p: Vec<String> = params
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
            Type::Primitive(p) => p.display().to_string(),
            Type::Parameter(name) => name.clone(),
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

/// Checks if a two-segment qualified type path like `json.Decoder` is valid.
/// Retained for backward compatibility within [`resolve_type_expr_full`] which
/// still receives a standalone map. Prefer [`TypeContext::is_package_type`]
/// in all new code.
fn is_package_type(
    package: &str,
    type_name: &str,
    package_types: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    package_types
        .get(package)
        .is_some_and(|types| types.contains(type_name))
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
) -> Type {
    resolve_type_expr_full(
        type_expr,
        known_structs,
        known_enums,
        known_type_params,
        known_type_aliases,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
}

/// Resolves a type expression with full context including package-qualified types
/// and file-private module aliases.
pub fn resolve_type_expr_full(
    type_expr: &TypeExpr,
    known_structs: &[&str],
    known_enums: &[&str],
    known_type_params: &[&str],
    known_type_aliases: &BTreeMap<String, Type>,
    package_types: &BTreeMap<String, BTreeSet<String>>,
    module_aliases: &BTreeMap<String, Type>,
) -> Type {
    match type_expr {
        TypeExpr::Generic { path, args, .. } => {
            let base_name = if path.len() == 1 {
                Some(path[0].as_str())
            } else if path.len() == 2 && is_package_type(&path[0], &path[1], package_types) {
                Some(path[1].as_str())
            } else {
                None
            };
            if let Some(name) = base_name
                && (known_structs.contains(&name) || known_enums.contains(&name))
            {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| {
                        resolve_type_expr_full(
                            a,
                            known_structs,
                            known_enums,
                            known_type_params,
                            known_type_aliases,
                            package_types,
                            module_aliases,
                        )
                    })
                    .collect();
                return Type::Named {
                    identifier: TypeIdentifier::unresolved(name),
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
            } else if path.len() == 2 && is_package_type(&path[0], &path[1], package_types) {
                let name = path[1].as_str();
                if known_structs.contains(&name) || known_enums.contains(&name) {
                    Type::Named {
                        identifier: TypeIdentifier::unresolved(name),
                        type_args: vec![],
                    }
                } else {
                    Type::Unknown
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
                        package_types,
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
                package_types,
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
                        package_types,
                        module_aliases,
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
                let mangled = mangle_name(&identifier.name, &substituted);
                Type::Named {
                    identifier: TypeIdentifier {
                        package: identifier.package.clone(),
                        name: mangled,
                    },
                    type_args: vec![],
                }
            }
        }
        Type::Indirect(inner) => Type::Indirect(Box::new(substitute(inner, subst))),
        Type::Union(members) => Type::union(members.iter().map(|m| substitute(m, subst)).collect()),
        _ => ty.clone(),
    }
}

/// Like [`substitute`], but preserves [`Type::Named`] with type args instead of
/// collapsing fully-resolved instances to mangled names.
/// Used by `resolve_type_expr` so downstream code can inspect the structured
/// generic form without re-parsing mangled names.
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
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                identifier.mangled()
            } else {
                mangle_name(&identifier.mangled(), type_args)
            }
        }
        Type::Parameter(n) => n.clone(),
        Type::Unit => "unit".to_string(),
        Type::Function {
            params,
            return_type,
        } => {
            let p: Vec<String> = params
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
            let parts: Vec<String> = members.iter().map(mangle_type).collect();
            format!("Union_${}$", parts.join("."))
        }
        _ => "unknown".to_string(),
    }
}

/// Builds a substitution map from type parameter names to concrete type arguments.
pub fn build_substitution(type_params: &[TypeParam], type_args: &[Type]) -> HashMap<String, Type> {
    type_params
        .iter()
        .zip(type_args.iter())
        .map(|(tp, ta)| (tp.name.clone(), ta.clone()))
        .collect()
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
        identifier: TypeIdentifier::unresolved("Pair"),
        type_args: vec![
            m.clone(),
            Type::Named {
                identifier: TypeIdentifier::unresolved("Option"),
                type_args: vec![Type::Named {
                    identifier: TypeIdentifier::unresolved("ReplyTo"),
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

/// Helper to construct a generic Named type with an unresolved package.
pub fn named_generic(name: &str, type_args: Vec<Type>) -> Type {
    Type::Named {
        identifier: TypeIdentifier::unresolved(name),
        type_args,
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
        let ty = named_generic("Option", vec![Type::Primitive(Primitive::I64)]);
        assert_eq!(ty.display(), "Option<Int>");
    }

    #[test]
    fn display_nested_generic() {
        let ty = named_generic(
            "Result",
            vec![
                named_generic("List", vec![Type::Primitive(Primitive::I64)]),
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
