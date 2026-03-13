use std::collections::HashMap;

use expo_ast::ast::Diagnostic;
use expo_ast::span::Span;

use crate::types::Type;

pub struct TypeContext {
    pub diagnostics: Vec<Diagnostic>,
    pub enums: HashMap<String, EnumInfo>,
    pub functions: HashMap<String, FunctionSig>,
    pub loop_depth: usize,
    pub structs: HashMap<String, StructInfo>,
}

pub struct EnumInfo {
    pub methods: HashMap<String, FunctionSig>,
    #[allow(dead_code)]
    pub span: Span,
    pub variants: Vec<VariantInfo>,
}

pub struct FunctionSig {
    pub params: Vec<ParamInfo>,
    pub return_type: Type,
    #[allow(dead_code)]
    pub span: Span,
}

pub struct ParamInfo {
    pub name: String,
    pub ty: Type,
}

pub struct StructInfo {
    pub fields: Vec<(String, Type)>,
    pub methods: HashMap<String, FunctionSig>,
    #[allow(dead_code)]
    pub span: Span,
}

pub struct VariantInfo {
    pub data: VariantData,
    pub name: String,
}

#[derive(Clone)]
pub enum VariantData {
    Struct(Vec<(String, Type)>),
    Tuple(Vec<Type>),
    Unit,
}

impl TypeContext {
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            enums: HashMap::new(),
            functions: HashMap::new(),
            loop_depth: 0,
            structs: HashMap::new(),
        }
    }

    pub fn error(&mut self, message: String, span: Span) {
        self.diagnostics.push(Diagnostic {
            message,
            hint: None,
            span,
        });
    }
}
