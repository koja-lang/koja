use std::collections::HashMap;

use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, ImplBlock, ProtocolDecl, Severity, StructDecl,
};
pub use expo_ast::ast::{PassMode, Visibility};
use expo_ast::span::Span;

use crate::types::Type;

/// Holds all type information gathered during collection and checking for a single module.
#[derive(Clone)]
pub struct TypeContext {
    pub closure_captures: HashMap<Span, Vec<CaptureInfo>>,
    pub constants: HashMap<String, Type>,
    pub diagnostics: Vec<Diagnostic>,
    pub enums: HashMap<String, EnumInfo>,
    pub functions: HashMap<String, FunctionSig>,
    pub generic_enum_asts: HashMap<String, EnumDecl>,
    pub generic_function_asts: HashMap<String, Function>,
    pub generic_impl_asts: HashMap<String, Vec<ImplBlock>>,
    pub generic_protocol_asts: HashMap<String, ProtocolDecl>,
    pub generic_struct_asts: HashMap<String, StructDecl>,
    pub imported_modules: HashMap<String, TypeContext>,
    pub process_fn_msg_types: HashMap<String, Type>,
    pub protocol_impls: HashMap<String, Vec<String>>,
    pub protocols: HashMap<String, ProtocolInfo>,
    pub structs: HashMap<String, StructInfo>,
    pub type_aliases: HashMap<String, Type>,
}

/// Collected metadata for an enum declaration.
#[derive(Clone)]
pub struct EnumInfo {
    pub methods: HashMap<String, FunctionSig>,
    #[allow(dead_code)]
    pub span: Span,
    pub type_params: Vec<String>,
    pub variants: Vec<VariantInfo>,
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
    #[allow(dead_code)]
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
    pub methods: HashMap<String, FunctionSig>,
    #[allow(dead_code)]
    pub span: Span,
    pub type_params: Vec<String>,
}

/// Collected metadata for a struct declaration.
#[derive(Clone)]
pub struct StructInfo {
    pub fields: Vec<(String, Type)>,
    pub methods: HashMap<String, FunctionSig>,
    #[allow(dead_code)]
    pub span: Span,
    pub type_params: Vec<String>,
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

    /// Creates an empty context with no registered types or diagnostics.
    pub fn new() -> Self {
        Self {
            closure_captures: HashMap::new(),
            constants: HashMap::new(),
            diagnostics: Vec::new(),
            enums: HashMap::new(),
            functions: HashMap::new(),
            generic_enum_asts: HashMap::new(),
            generic_function_asts: HashMap::new(),
            generic_impl_asts: HashMap::new(),
            generic_protocol_asts: HashMap::new(),
            generic_struct_asts: HashMap::new(),
            imported_modules: HashMap::new(),
            process_fn_msg_types: HashMap::new(),
            protocol_impls: HashMap::new(),
            protocols: HashMap::new(),
            structs: HashMap::new(),
            type_aliases: HashMap::new(),
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
        for (name, info) in &other.structs {
            if !self.structs.contains_key(name) {
                self.structs.insert(name.clone(), info.clone());
            }
        }
        for (name, info) in &other.enums {
            if !self.enums.contains_key(name) {
                self.enums.insert(name.clone(), info.clone());
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
        for (type_name, protos) in &other.protocol_impls {
            self.protocol_impls
                .entry(type_name.clone())
                .or_default()
                .extend(protos.iter().cloned());
        }
        for (name, ty) in &other.type_aliases {
            if !self.type_aliases.contains_key(name) {
                self.type_aliases.insert(name.clone(), ty.clone());
            }
        }
        for (span, captures) in &other.closure_captures {
            self.closure_captures.insert(*span, captures.clone());
        }
        for (name, ty) in &other.process_fn_msg_types {
            self.process_fn_msg_types
                .entry(name.clone())
                .or_insert_with(|| ty.clone());
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
