use std::collections::{BTreeMap, HashMap};

use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, ImplBlock, ProtocolDecl, ProtocolMethod, Severity, StructDecl,
    TypeParam,
};
pub use expo_ast::ast::{PassMode, Visibility};
use expo_ast::span::Span;

pub use crate::types::{FnParam, Type, TypeIdentifier};

pub type SpecializedMethodMap =
    BTreeMap<TypeIdentifier, Vec<(Vec<Type>, BTreeMap<String, FunctionSig>)>>;

/// Holds all type information gathered during collection and checking for a single module.
#[derive(Clone)]
pub struct TypeContext {
    pub closure_info: HashMap<Span, ClosureInfo>,
    pub coercions: HashMap<Span, Coercion>,
    pub constants: BTreeMap<String, Type>,
    pub diagnostics: Vec<Diagnostic>,
    pub functions: BTreeMap<String, FunctionSig>,
    pub generic_enum_asts: BTreeMap<String, EnumDecl>,
    pub generic_function_asts: BTreeMap<String, Function>,
    pub generic_impl_asts: BTreeMap<String, Vec<ImplBlock>>,
    pub generic_protocol_asts: BTreeMap<String, ProtocolDecl>,
    pub generic_struct_asts: BTreeMap<String, StructDecl>,
    pub protocol_impls: BTreeMap<String, Vec<(String, Vec<Type>)>>,
    pub protocols: BTreeMap<String, ProtocolInfo>,
    pub specialized_impl_asts: BTreeMap<TypeIdentifier, Vec<(Vec<Type>, ImplBlock)>>,
    pub specialized_methods: SpecializedMethodMap,
    pub synthesized_default_fns: BTreeMap<String, Vec<Function>>,
    pub type_aliases: BTreeMap<String, Type>,
    pub types: BTreeMap<TypeIdentifier, TypeInfo>,
    /// File-private aliases from `alias` declarations. NOT merged across modules.
    pub module_aliases: BTreeMap<String, Type>,
    /// Reverse index from bare type name to its fully qualified
    /// [`TypeIdentifier`]. Populated by the resolution pass; empty before that.
    pub name_index: BTreeMap<String, TypeIdentifier>,
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
        self.types
            .values()
            .any(|ti| ti.identifier.name == name && ti.is_struct())
    }

    /// Returns `true` if `name` is registered as an enum in the type registry.
    pub fn is_enum(&self, name: &str) -> bool {
        self.types
            .values()
            .any(|ti| ti.identifier.name == name && ti.is_enum())
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
    /// using the reverse index built by the resolution pass.
    pub fn resolve_name(&self, name: &str) -> Option<&TypeIdentifier> {
        self.name_index.get(name)
    }

    /// Looks up a type by bare name: resolves the name to a [`TypeIdentifier`],
    /// then fetches the corresponding [`TypeInfo`].
    pub fn find_type(&self, name: &str) -> Option<&TypeInfo> {
        self.resolve_name(name).and_then(|id| self.get_type(id))
    }

    /// Resolves `Package::Unresolved` identifiers inside a [`Type`] using the
    /// name index built by the resolution pass.
    pub fn resolve_type(&self, ty: &mut Type) {
        crate::resolve::resolve_type_inline(ty, &self.name_index);
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
                self.types.insert(id.clone(), info.clone());
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
        for (span, info) in &other.closure_info {
            self.closure_info.insert(*span, info.clone());
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
