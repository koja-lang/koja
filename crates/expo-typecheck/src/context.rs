use std::collections::HashMap;

pub use expo_ast::ast::{PassMode, Visibility};
use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, ImplBlock, ProtocolDecl, Severity, StructDecl,
};
use expo_ast::span::Span;

use crate::types::Type;

/// Holds all type information gathered during collection and checking for a single module.
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
    pub protocol_impls: HashMap<String, Vec<String>>,
    pub protocols: HashMap<String, ProtocolInfo>,
    pub structs: HashMap<String, StructInfo>,
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

/// Resolved type signature for a function or method.
#[derive(Clone)]
pub struct FunctionSig {
    pub visibility: Visibility,
    pub params: Vec<ParamInfo>,
    pub return_type: Type,
    /// How the receiver (`self`) is passed: `Move` for `move self`, `Borrow` otherwise.
    pub self_mode: PassMode,
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
            protocol_impls: HashMap::new(),
            protocols: HashMap::new(),
            structs: HashMap::new(),
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
