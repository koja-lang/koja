use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, ImplBlock, ProtocolDecl, ProtocolMethod, Severity, StructDecl,
    TypeExpr, TypeParam,
};
pub use expo_ast::ast::{PassMode, Visibility};
use expo_ast::span::Span;

use crate::types::resolve_type_expr_full;
pub use crate::types::{FnParam, Package, Type, TypeIdentifier};

pub type SpecializedMethodMap =
    BTreeMap<TypeIdentifier, Vec<(Vec<Type>, BTreeMap<String, FunctionSig>)>>;

/// Holds all type information gathered during collection and checking for a single module.
#[derive(Clone)]
pub struct TypeContext {
    /// Keyed by `(source file path, closure span)` so merged graphs from many
    /// modules do not collide on identical line/column positions.
    pub closure_info: HashMap<(Option<PathBuf>, Span), ClosureInfo>,
    /// Set while [`crate::check_module`] walks a module; used when recording
    /// [`ClosureInfo`] keys. Not meaningful in merged contexts.
    pub current_module_path: Option<PathBuf>,
    /// The package whose source is currently being type-checked. Bare-name
    /// type lookups (e.g. `find_type("Config")`) consult this first so that
    /// references inside a file always prefer their own package's definition
    /// over a colliding type in a dependency. Reset to `None` outside an
    /// active check.
    pub current_package: Option<Package>,
    pub coercions: HashMap<Span, Coercion>,
    pub constants: BTreeMap<TypeIdentifier, Type>,
    pub diagnostics: Vec<Diagnostic>,
    pub functions: BTreeMap<String, FunctionSig>,
    pub generic_enum_asts: BTreeMap<String, EnumDecl>,
    pub generic_function_asts: BTreeMap<String, Function>,
    pub generic_impl_asts: BTreeMap<String, Vec<ImplBlock>>,
    pub generic_protocol_asts: BTreeMap<String, ProtocolDecl>,
    pub generic_struct_asts: BTreeMap<String, StructDecl>,
    pub protocol_impls: BTreeMap<TypeIdentifier, Vec<(String, Vec<Type>)>>,
    pub protocols: BTreeMap<String, ProtocolInfo>,
    pub specialized_impl_asts: BTreeMap<TypeIdentifier, Vec<(Vec<Type>, ImplBlock)>>,
    pub specialized_methods: SpecializedMethodMap,
    pub synthesized_default_fns: BTreeMap<String, Vec<Function>>,
    pub type_aliases: BTreeMap<String, Type>,
    pub types: BTreeMap<TypeIdentifier, TypeInfo>,
    /// File-private aliases from `alias` declarations. NOT merged across modules.
    pub module_aliases: BTreeMap<String, Type>,
    /// Reverse index from type name to its fully qualified [`TypeIdentifier`].
    /// Contains both qualified (`"std.Option"`) and bare (`"Option"`) entries.
    /// Populated by the resolution pass; empty before that.
    pub name_index: BTreeMap<String, TypeIdentifier>,
    /// Package-to-type-names index for resolving qualified type paths like
    /// `http.Request`. Populated by the resolution pass alongside `name_index`.
    pub package_types: BTreeMap<Package, BTreeSet<String>>,
}

/// Whether a function in an impl block takes a `self` receiver or is static.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    /// An instance function that takes `self`. The [`PassMode`] indicates
    /// whether `self` is borrowed (read-only) or moved (owned).
    Instance(PassMode),
    /// A static function with no `self` receiver, called as `Type.function()`.
    Static,
}

/// Resolved type signature for a function or method.
#[derive(Clone, PartialEq)]
pub struct FunctionSig {
    pub visibility: Visibility,
    pub params: Vec<ParamInfo>,
    pub return_type: Type,
    pub kind: FunctionKind,
    pub span: Span,
    pub type_params: Vec<TypeParam>,
}

/// A single parameter's name, resolved type, and how ownership is transferred.
#[derive(Clone, PartialEq)]
pub struct ParamInfo {
    pub mode: PassMode,
    pub name: String,
    pub ty: Type,
}

impl From<&ParamInfo> for FnParam {
    fn from(p: &ParamInfo) -> Self {
        Self {
            ty: p.ty.clone(),
            mode: p.mode,
        }
    }
}

/// All type-checker metadata for a single closure (block or short).
#[derive(Clone)]
pub struct ClosureInfo {
    pub captures: Vec<CaptureInfo>,
    pub param_types: Vec<Type>,
    pub return_type: Option<Type>,
}

/// Collected metadata for a protocol declaration.
#[derive(Clone)]
pub struct ProtocolInfo {
    pub default_bodies: BTreeMap<String, ProtocolMethod>,
    pub methods: BTreeMap<String, FunctionSig>,
    pub span: Span,
    pub type_params: Vec<TypeParam>,
}

/// Unified metadata for any named type: struct, enum, or primitive.
/// Functions (Expo's term for methods) and type parameters live here
/// regardless of the type's kind. The [`TypeKind`] discriminator carries
/// kind-specific data (fields for structs, variants for enums).
#[derive(Clone, PartialEq)]
pub struct TypeInfo {
    pub identifier: TypeIdentifier,
    pub functions: BTreeMap<String, FunctionSig>,
    pub kind: TypeKind,
    pub span: Span,
    pub type_params: Vec<TypeParam>,
}

/// What kind of named type a [`TypeInfo`] represents.
#[derive(Clone, PartialEq)]
pub enum TypeKind {
    Struct { fields: Vec<(String, Type)> },
    Enum { variants: Vec<VariantInfo> },
    Primitive,
}

impl TypeInfo {
    /// Returns `true` if this type info describes a struct.
    pub fn is_struct(&self) -> bool {
        matches!(self.kind, TypeKind::Struct { .. })
    }

    /// Returns `true` if this type info describes an enum.
    pub fn is_enum(&self) -> bool {
        matches!(self.kind, TypeKind::Enum { .. })
    }

    /// Returns the struct's fields, or `None` if this is not a struct.
    pub fn fields(&self) -> Option<&Vec<(String, Type)>> {
        if let TypeKind::Struct { fields } = &self.kind {
            Some(fields)
        } else {
            None
        }
    }

    /// Returns a mutable reference to the struct's fields, or `None` if not a struct.
    pub fn fields_mut(&mut self) -> Option<&mut Vec<(String, Type)>> {
        if let TypeKind::Struct { fields } = &mut self.kind {
            Some(fields)
        } else {
            None
        }
    }

    /// Returns the enum's variants, or `None` if this is not an enum.
    pub fn variants(&self) -> Option<&Vec<VariantInfo>> {
        if let TypeKind::Enum { variants } = &self.kind {
            Some(variants)
        } else {
            None
        }
    }

    /// Returns a mutable reference to the enum's variants, or `None` if not an enum.
    pub fn variants_mut(&mut self) -> Option<&mut Vec<VariantInfo>> {
        if let TypeKind::Enum { variants } = &mut self.kind {
            Some(variants)
        } else {
            None
        }
    }

    /// Returns a human-readable label for the type kind: `"struct"`, `"enum"`, or `"type"`.
    pub fn kind_label(&self) -> &'static str {
        match &self.kind {
            TypeKind::Struct { .. } => "struct",
            TypeKind::Enum { .. } => "enum",
            TypeKind::Primitive => "type",
        }
    }
}

/// A single variant within an enum.
#[derive(Clone, PartialEq)]
pub struct VariantInfo {
    pub data: VariantData,
    pub name: String,
}

/// The shape of data carried by an enum variant.
#[derive(Clone, PartialEq)]
pub enum VariantData {
    Struct(Vec<(String, Type)>),
    Tuple(Vec<Type>),
    Unit,
}

/// A single variable captured by a closure.
#[derive(Debug, Clone)]
pub struct CaptureInfo {
    pub name: String,
    pub ty: Type,
    pub mode: PassMode,
}

/// A type coercion recorded by the type checker for the codegen to apply.
#[derive(Debug, Clone)]
pub enum Coercion {
    /// A value of `source` type is widened into a `target` union type.
    UnionWiden { source: Type, target: Type },
}

impl Default for TypeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeContext {
    /// Records an error diagnostic at the given span.
    pub fn error(&mut self, message: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            message,
            hint: None,
            span,
        });
    }

    /// Records an error diagnostic with an additional hint message.
    pub fn error_with_hint(&mut self, message: String, hint: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            message,
            hint: Some(hint),
            span,
        });
    }

    /// Returns `true` if `name` is registered as a struct in the type registry.
    pub fn is_struct(&self, name: &str) -> bool {
        self.lookup_by_name(name).is_some_and(|ti| ti.is_struct())
    }

    /// Returns `true` if `name` is registered as an enum in the type registry.
    pub fn is_enum(&self, name: &str) -> bool {
        self.lookup_by_name(name).is_some_and(|ti| ti.is_enum())
    }

    /// Scope-aware [`TypeInfo`] lookup by bare name. Prefers the current
    /// package's definition (when set), then falls back to `std`. Returns
    /// `None` for cross-package dependency types — callers must use the
    /// qualified form or declare an `alias` to reach them.
    pub fn lookup_by_name(&self, name: &str) -> Option<&TypeInfo> {
        if let Some(Package::Named(pkg)) = &self.current_package
            && let Some(ti) = self.types.get(&TypeIdentifier::new(pkg, name))
        {
            return Some(ti);
        }
        self.types.get(&TypeIdentifier::std(name))
    }

    /// Collects the names of all registered struct types.
    pub fn struct_names(&self) -> Vec<String> {
        self.types
            .values()
            .filter(|ti| ti.is_struct())
            .map(|ti| ti.identifier.name.clone())
            .collect()
    }

    /// Collects the names of all registered enum types.
    pub fn enum_names(&self) -> Vec<String> {
        self.types
            .values()
            .filter(|ti| ti.is_enum())
            .map(|ti| ti.identifier.name.clone())
            .collect()
    }

    /// Iterator over the bare names of every registered struct or enum type.
    /// Useful for callers that need to test "is this a known user-defined
    /// type name?" without distinguishing struct vs enum.
    pub fn struct_and_enum_names(&self) -> impl Iterator<Item = &str> {
        self.types
            .values()
            .filter(|ti| ti.is_struct() || ti.is_enum())
            .map(|ti| ti.identifier.name.as_str())
    }

    /// Returns true if any registered type lives in the named package.
    /// Names like `"std"` are matched against the synthetic [`Package::Std`]
    /// variant, not [`Package::Named("std")`], so use [`Package::matches_name`]
    /// indirectly via this helper instead of pattern-matching at call sites.
    pub fn has_named_package(&self, package: &str) -> bool {
        self.types.keys().any(|id| match &id.package {
            Package::Named(p) => p == package,
            _ => false,
        })
    }

    /// Returns true if a struct/enum/alias with `name` exists in the named
    /// (non-`std`) package `package`.
    pub fn has_type_in_named_package(&self, package: &str, name: &str) -> bool {
        self.types
            .keys()
            .any(|id| matches!(&id.package, Package::Named(p) if p == package && id.name == name))
    }

    /// Looks up a function signature by its (possibly mangled) name.
    pub fn function_sig(&self, name: &str) -> Option<&FunctionSig> {
        self.functions.get(name)
    }

    /// Inserts a type into the registry keyed by its [`TypeIdentifier`].
    pub fn insert_type(&mut self, id: TypeIdentifier, mut info: TypeInfo) {
        info.identifier = id.clone();
        self.types.insert(id, info);
    }

    /// Returns the [`TypeInfo`] for the given [`TypeIdentifier`].
    pub fn get_type(&self, id: &TypeIdentifier) -> Option<&TypeInfo> {
        self.types.get(id)
    }

    /// Returns a mutable reference to the [`TypeInfo`] for the given [`TypeIdentifier`].
    pub fn get_type_mut(&mut self, id: &TypeIdentifier) -> Option<&mut TypeInfo> {
        self.types.get_mut(id)
    }

    /// Resolves a bare type name to its fully qualified [`TypeIdentifier`]
    /// using the reverse index built by the resolution pass. If the context
    /// has a `current_package` set, that package's qualified entry wins over
    /// the shared bare entry (which is last-write-wins).
    pub fn resolve_name(&self, name: &str) -> Option<&TypeIdentifier> {
        if let Some(scope) = &self.current_package
            && let Some(id) = self.resolve_name_scoped(name, scope)
        {
            return Some(id);
        }
        self.name_index.get(name)
    }

    /// Resolves `name` preferring `scope.name` (package-qualified) over the
    /// shared bare entry. Returns `None` when neither lookup matches.
    pub fn resolve_name_scoped(&self, name: &str, scope: &Package) -> Option<&TypeIdentifier> {
        scope
            .qualify(name)
            .and_then(|q| self.name_index.get(&q))
            .or_else(|| self.name_index.get(name))
    }

    /// Looks up a type by bare name: resolves the name to a [`TypeIdentifier`],
    /// then fetches the corresponding [`TypeInfo`].
    pub fn find_type(&self, name: &str) -> Option<&TypeInfo> {
        self.resolve_name(name).and_then(|id| self.get_type(id))
    }

    /// Scope-aware variant of [`Self::find_type`] that ignores any ambient
    /// `current_package` and uses the caller-provided scope instead. Used by
    /// resolution passes that walk `TypeInfo`s whose container package differs
    /// from the context's ambient scope.
    pub fn find_type_scoped(&self, name: &str, scope: &Package) -> Option<&TypeInfo> {
        self.resolve_name_scoped(name, scope)
            .and_then(|id| self.get_type(id))
    }

    /// Returns the type-argument slice of the `Process` protocol implementation
    /// for the type identified by `id`, or `None` when the type does not
    /// implement `Process`. The first argument is the implementing type, the
    /// second is the message type, and the third is the reply type.
    pub fn process_impl_args(&self, id: &TypeIdentifier) -> Option<&[Type]> {
        self.protocol_impls
            .get(id)?
            .iter()
            .find(|(proto, _)| proto == "Process")
            .map(|(_, args)| args.as_slice())
    }

    /// Convenience over [`Self::process_impl_args`]: returns the
    /// `process_envelope` (msg/reply) type for the given impl, when both
    /// arguments are present.
    pub fn process_envelope_for(&self, id: &TypeIdentifier) -> Option<Type> {
        let args = self.process_impl_args(id)?;
        let msg = args.get(1)?;
        let reply = args.get(2)?;
        Some(crate::types::process_envelope_type(msg, reply))
    }

    /// Resolves `Package::Unresolved` identifiers inside a [`Type`] using the
    /// name index built by the resolution pass. When the context has a
    /// `current_package` set, the active scope is threaded through so bare
    /// references prefer the scope's own definition over a colliding entry.
    pub fn resolve_type(&self, ty: &mut Type) {
        if let Some(scope) = &self.current_package {
            crate::resolve::resolve_type_inline_scoped(ty, &self.name_index, scope);
        } else {
            crate::resolve::resolve_type_inline(ty, &self.name_index);
        }
    }

    /// Resolves a [`TypeExpr`] annotation using the full cached context: type
    /// aliases, module aliases, and package-qualified type lookups. Replaces the
    /// `resolve_type_expr(...) + ctx.resolve_type(...)` pattern at call sites.
    pub fn resolve_type_annotation(
        &self,
        te: &TypeExpr,
        struct_names: &[&str],
        enum_names: &[&str],
    ) -> Type {
        let known_packages: BTreeSet<Package> = self.package_types.keys().cloned().collect();
        let mut ty = resolve_type_expr_full(
            te,
            struct_names,
            enum_names,
            &[],
            &self.type_aliases,
            &known_packages,
            &self.module_aliases,
        );
        self.resolve_type(&mut ty);
        ty
    }

    /// Returns `true` if the given package provides a type with the given name.
    pub fn is_package_type(&self, pkg: &str, type_name: &str) -> bool {
        let id = if pkg == "std" {
            TypeIdentifier::std(type_name)
        } else {
            TypeIdentifier::new(pkg, type_name)
        };
        self.types.contains_key(&id)
    }

    /// Creates an empty context with no registered types or diagnostics.
    pub fn new() -> Self {
        Self {
            closure_info: HashMap::new(),
            current_module_path: None,
            current_package: None,
            coercions: HashMap::new(),
            constants: BTreeMap::new(),
            diagnostics: Vec::new(),
            functions: BTreeMap::new(),
            generic_enum_asts: BTreeMap::new(),
            generic_function_asts: BTreeMap::new(),
            generic_impl_asts: BTreeMap::new(),
            generic_protocol_asts: BTreeMap::new(),
            generic_struct_asts: BTreeMap::new(),
            protocol_impls: BTreeMap::new(),
            protocols: BTreeMap::new(),
            specialized_impl_asts: BTreeMap::new(),
            specialized_methods: BTreeMap::new(),
            synthesized_default_fns: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            types: BTreeMap::new(),
            module_aliases: BTreeMap::new(),
            name_index: BTreeMap::new(),
            package_types: BTreeMap::new(),
        }
    }

    /// Merges all type information from `other` into `self`. Entries already
    /// present in `self` are kept (first-writer-wins), except for
    /// `generic_impl_asts`, `specialized_impl_asts`, `specialized_methods`,
    /// and `protocol_impls` which accumulate across modules.
    ///
    /// When two types share the same bare name but come from different packages,
    /// the first-writer (already in `self`) wins for the flat namespace. Functions
    /// are only merged when the types come from the same package (or either side
    /// has an unresolved package, for backwards compatibility during migration).
    pub fn merge(&mut self, other: &TypeContext) {
        for (name, sig) in &other.functions {
            if !self.functions.contains_key(name) {
                self.functions.insert(name.clone(), sig.clone());
            }
        }
        for (id, info) in &other.types {
            if let Some(existing) = self.types.get_mut(id) {
                for (fn_name, sig) in &info.functions {
                    if !existing.functions.contains_key(fn_name) {
                        existing.functions.insert(fn_name.clone(), sig.clone());
                    }
                }
            } else {
                let empty_key = TypeIdentifier::new("", &id.name);
                if id.package != Package::Named(String::new())
                    && self.types.contains_key(&empty_key)
                {
                    // self has Named("", X) from the current buffer while other
                    // has the real package (e.g. Named("net", X) from stdlib).
                    // Promote to the real package, letting current-buffer
                    // functions win so live edits are reflected immediately.
                    let mut merged = info.clone();
                    if let Some(current) = self.types.remove(&empty_key) {
                        for (fn_name, sig) in current.functions {
                            merged.functions.insert(fn_name, sig);
                        }
                    }
                    self.types.insert(id.clone(), merged);
                } else {
                    self.types.insert(id.clone(), info.clone());
                }
            }
        }
        for (name, ast) in &other.generic_function_asts {
            if !self.generic_function_asts.contains_key(name) {
                self.generic_function_asts.insert(name.clone(), ast.clone());
            }
        }
        for (name, ast) in &other.generic_struct_asts {
            if !self.generic_struct_asts.contains_key(name) {
                self.generic_struct_asts.insert(name.clone(), ast.clone());
            }
        }
        for (name, ast) in &other.generic_enum_asts {
            if !self.generic_enum_asts.contains_key(name) {
                self.generic_enum_asts.insert(name.clone(), ast.clone());
            }
        }
        for (name, blocks) in &other.generic_impl_asts {
            let existing = self.generic_impl_asts.entry(name.clone()).or_default();
            for block in blocks {
                let dominated = existing.iter().any(|b| b.span == block.span);
                if !dominated {
                    existing.push(block.clone());
                }
            }
        }
        for (id, entries) in &other.specialized_impl_asts {
            let existing = self.specialized_impl_asts.entry(id.clone()).or_default();
            for entry in entries {
                let dominated = existing.iter().any(|e| e.1.span == entry.1.span);
                if !dominated {
                    existing.push(entry.clone());
                }
            }
        }
        for (id, entries) in &other.specialized_methods {
            let existing = self.specialized_methods.entry(id.clone()).or_default();
            for entry in entries {
                let dominated = existing.iter().any(|e| e.0 == entry.0);
                if !dominated {
                    existing.push(entry.clone());
                }
            }
        }
        for (name, ast) in &other.generic_protocol_asts {
            if !self.generic_protocol_asts.contains_key(name) {
                self.generic_protocol_asts.insert(name.clone(), ast.clone());
            }
        }
        for (name, info) in &other.protocols {
            if !self.protocols.contains_key(name) {
                self.protocols.insert(name.clone(), info.clone());
            }
        }
        for (type_name, impls) in &other.protocol_impls {
            let existing = self.protocol_impls.entry(type_name.clone()).or_default();
            for entry in impls {
                if !existing.contains(entry) {
                    existing.push(entry.clone());
                }
            }
        }
        for (type_name, fns) in &other.synthesized_default_fns {
            let existing = self
                .synthesized_default_fns
                .entry(type_name.clone())
                .or_default();
            for f in fns {
                let dominated = existing.iter().any(|e| e.span == f.span);
                if !dominated {
                    existing.push(f.clone());
                }
            }
        }
        for (name, ty) in &other.type_aliases {
            if !self.type_aliases.contains_key(name) {
                self.type_aliases.insert(name.clone(), ty.clone());
            }
        }
        for (name, ty) in &other.constants {
            if !self.constants.contains_key(name) {
                self.constants.insert(name.clone(), ty.clone());
            }
        }
        for ((path, span), info) in &other.closure_info {
            self.closure_info
                .insert((path.clone(), *span), info.clone());
        }
        for (span, coercion) in &other.coercions {
            self.coercions
                .entry(*span)
                .or_insert_with(|| coercion.clone());
        }
        // module_aliases intentionally NOT merged (file-private)
    }

    /// Records a warning diagnostic at the given span.
    pub fn warning(&mut self, message: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            message,
            hint: None,
            span,
        });
    }

    /// Records a warning diagnostic with an additional hint at the given span.
    pub fn warning_with_hint(&mut self, message: String, hint: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            message,
            hint: Some(hint),
            span,
        });
    }
}
