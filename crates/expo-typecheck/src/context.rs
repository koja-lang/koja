use std::collections::HashMap;

use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, ImplBlock, ProtocolDecl, ProtocolMethod, Severity, StructDecl,
};
pub use expo_ast::ast::{PassMode, Visibility};
use expo_ast::span::Span;

use crate::types::Type;

/// Holds all type information gathered during collection and checking for a single module.
#[derive(Clone)]
pub struct TypeContext {
    pub closure_captures: HashMap<Span, Vec<CaptureInfo>>,
    pub coercions: HashMap<Span, Coercion>,
    pub constants: HashMap<String, Type>,
    pub diagnostics: Vec<Diagnostic>,
    pub functions: HashMap<String, FunctionSig>,
    pub generic_enum_asts: HashMap<String, EnumDecl>,
    pub generic_function_asts: HashMap<String, Function>,
    pub generic_impl_asts: HashMap<String, Vec<ImplBlock>>,
    pub generic_protocol_asts: HashMap<String, ProtocolDecl>,
    pub generic_struct_asts: HashMap<String, StructDecl>,
    pub imported_modules: HashMap<String, TypeContext>,
    pub protocol_impls: HashMap<String, Vec<(String, Vec<Type>)>>,
    pub protocols: HashMap<String, ProtocolInfo>,
    pub synthesized_default_fns: HashMap<String, Vec<Function>>,
    pub type_aliases: HashMap<String, Type>,
    pub types: HashMap<String, TypeInfo>,
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
#[derive(Clone)]
pub struct FunctionSig {
    pub visibility: Visibility,
    pub params: Vec<ParamInfo>,
    pub return_type: Type,
    pub kind: FunctionKind,
    pub span: Span,
    pub type_params: Vec<String>,
}

/// A single parameter's name, resolved type, and how ownership is transferred.
#[derive(Clone)]
pub struct ParamInfo {
    pub mode: PassMode,
    pub name: String,
    pub ty: Type,
}

/// Collected metadata for a protocol declaration.
#[derive(Clone)]
pub struct ProtocolInfo {
    pub default_bodies: HashMap<String, ProtocolMethod>,
    pub methods: HashMap<String, FunctionSig>,
    pub span: Span,
    pub type_params: Vec<String>,
}

/// Unified metadata for any named type: struct, enum, or primitive.
/// Functions (Expo's term for methods) and type parameters live here
/// regardless of the type's kind. The [`TypeKind`] discriminator carries
/// kind-specific data (fields for structs, variants for enums).
#[derive(Clone)]
pub struct TypeInfo {
    pub functions: HashMap<String, FunctionSig>,
    pub kind: TypeKind,
    pub span: Span,
    pub type_params: Vec<String>,
}

/// What kind of named type a [`TypeInfo`] represents.
#[derive(Clone)]
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
#[derive(Clone)]
pub struct VariantInfo {
    pub data: VariantData,
    pub name: String,
}

/// The shape of data carried by an enum variant.
#[derive(Clone)]
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
        self.types.get(name).is_some_and(|ti| ti.is_struct())
    }

    /// Returns `true` if `name` is registered as an enum in the type registry.
    pub fn is_enum(&self, name: &str) -> bool {
        self.types.get(name).is_some_and(|ti| ti.is_enum())
    }

    /// Collects the names of all registered struct types.
    pub fn struct_names(&self) -> Vec<String> {
        self.types
            .iter()
            .filter(|(_, ti)| ti.is_struct())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Collects the names of all registered enum types.
    pub fn enum_names(&self) -> Vec<String> {
        self.types
            .iter()
            .filter(|(_, ti)| ti.is_enum())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Creates an empty context with no registered types or diagnostics.
    pub fn new() -> Self {
        Self {
            closure_captures: HashMap::new(),
            coercions: HashMap::new(),
            constants: HashMap::new(),
            diagnostics: Vec::new(),
            functions: HashMap::new(),
            generic_enum_asts: HashMap::new(),
            generic_function_asts: HashMap::new(),
            generic_impl_asts: HashMap::new(),
            generic_protocol_asts: HashMap::new(),
            generic_struct_asts: HashMap::new(),
            imported_modules: HashMap::new(),
            protocol_impls: HashMap::new(),
            protocols: HashMap::new(),
            synthesized_default_fns: HashMap::new(),
            type_aliases: HashMap::new(),
            types: HashMap::new(),
        }
    }

    /// Merges all type information from `other` into `self`. Entries already
    /// present in `self` are kept (first-writer-wins), except for
    /// `generic_impl_asts` and `protocol_impls` which accumulate across modules.
    pub fn merge(&mut self, other: &TypeContext) {
        for (name, sig) in &other.functions {
            if !self.functions.contains_key(name) {
                self.functions.insert(name.clone(), sig.clone());
            }
        }
        for (name, info) in &other.types {
            if let Some(existing) = self.types.get_mut(name) {
                for (fn_name, sig) in &info.functions {
                    if !existing.functions.contains_key(fn_name) {
                        existing.functions.insert(fn_name.clone(), sig.clone());
                    }
                }
            } else {
                self.types.insert(name.clone(), info.clone());
            }
        }
        for (mod_name, mod_ctx) in &other.imported_modules {
            if !self.imported_modules.contains_key(mod_name) {
                self.imported_modules
                    .insert(mod_name.clone(), mod_ctx.clone());
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
            self.generic_impl_asts
                .entry(name.clone())
                .or_default()
                .extend(blocks.iter().cloned());
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
            self.protocol_impls
                .entry(type_name.clone())
                .or_default()
                .extend(impls.iter().cloned());
        }
        for (type_name, fns) in &other.synthesized_default_fns {
            self.synthesized_default_fns
                .entry(type_name.clone())
                .or_default()
                .extend(fns.iter().cloned());
        }
        for (name, ty) in &other.type_aliases {
            if !self.type_aliases.contains_key(name) {
                self.type_aliases.insert(name.clone(), ty.clone());
            }
        }
        for (span, captures) in &other.closure_captures {
            self.closure_captures.insert(*span, captures.clone());
        }
        for (span, coercion) in &other.coercions {
            self.coercions
                .entry(*span)
                .or_insert_with(|| coercion.clone());
        }
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
}
