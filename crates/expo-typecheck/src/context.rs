use std::collections::HashMap;

use expo_ast::ast::Diagnostic;
use expo_ast::span::Span;

use crate::types::Type;

pub struct TypeContext {
    pub structs: HashMap<String, StructInfo>,
    pub functions: HashMap<String, FunctionSig>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct StructInfo {
    pub fields: Vec<(String, Type)>,
    #[allow(dead_code)]
    pub span: Span,
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

impl TypeContext {
    pub fn new() -> Self {
        Self {
            structs: HashMap::new(),
            functions: HashMap::new(),
            diagnostics: Vec::new(),
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
