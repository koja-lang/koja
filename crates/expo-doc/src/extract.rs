//! Walk the parsed AST and extract documentation items.

use expo_ast::ast::{
    AnnotationValue, EnumDecl, Function, ImplBlock, ImplMember, Item, Module, Param, StructDecl,
    TypeExpr,
};

/// Documentation for an entire module (source file).
#[derive(Debug)]
pub struct DocModule {
    pub name: String,
    pub moduledoc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub structs: Vec<DocStruct>,
    pub enums: Vec<DocEnum>,
    pub constants: Vec<DocConstant>,
}

/// Documentation for a function.
#[derive(Debug)]
pub struct DocFunction {
    pub name: String,
    pub params: Vec<DocParam>,
    pub return_type: Option<String>,
    pub doc: Option<String>,
}

/// A function parameter for display.
#[derive(Debug)]
pub struct DocParam {
    pub name: String,
    pub type_name: String,
}

/// Documentation for a struct, including its impl functions.
#[derive(Debug)]
pub struct DocStruct {
    pub name: String,
    pub fields: Vec<DocField>,
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
}

/// A struct field for display.
#[derive(Debug)]
pub struct DocField {
    pub name: String,
    pub type_name: String,
}

/// Documentation for an enum.
#[derive(Debug)]
pub struct DocEnum {
    pub name: String,
    pub variants: Vec<String>,
    pub doc: Option<String>,
}

/// Documentation for a constant.
#[derive(Debug)]
pub struct DocConstant {
    pub name: String,
    pub doc: Option<String>,
}

/// Extract documentation from a parsed module.
///
/// Returns `None` if the module has `@moduledoc false`, indicating it should
/// be excluded from generated documentation.
pub fn extract_module(name: &str, module: &Module) -> Option<DocModule> {
    if let Some(ref md) = module.moduledoc
        && md.value == Some(AnnotationValue::False)
    {
        return None;
    }

    let moduledoc = module.moduledoc.as_ref().and_then(|md| match &md.value {
        Some(AnnotationValue::String(s)) => Some(s.clone()),
        _ => None,
    });

    let mut functions = Vec::new();
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    let mut constants = Vec::new();

    for item in &module.items {
        match item {
            Item::Function(f) => {
                if !f.is_private
                    && let Some(df) = extract_function(f)
                {
                    functions.push(df);
                }
            }
            Item::Struct(s) => {
                if let Some(ds) = extract_struct(s) {
                    structs.push(ds);
                }
            }
            Item::Enum(e) => {
                if let Some(de) = extract_enum(e) {
                    enums.push(de);
                }
            }
            Item::Constant(c) => {
                if let Some(dc) = extract_constant(c) {
                    constants.push(dc);
                }
            }
            Item::Impl(imp) => {
                attach_impl_functions(imp, &mut structs);
            }
            _ => {}
        }
    }

    functions.sort_by(|a, b| a.name.cmp(&b.name));
    structs.sort_by(|a, b| a.name.cmp(&b.name));
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    constants.sort_by(|a, b| a.name.cmp(&b.name));

    for s in &mut structs {
        s.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Some(DocModule {
        name: name.to_string(),
        moduledoc,
        functions,
        structs,
        enums,
        constants,
    })
}

fn has_doc_false(annotation: &Option<expo_ast::ast::Annotation>) -> bool {
    annotation
        .as_ref()
        .is_some_and(|a| a.name == "doc" && a.value == Some(AnnotationValue::False))
}

fn annotation_string(annotation: &Option<expo_ast::ast::Annotation>) -> Option<String> {
    annotation.as_ref().and_then(|a| match &a.value {
        Some(AnnotationValue::String(s)) => Some(s.clone()),
        _ => None,
    })
}

fn extract_function(f: &Function) -> Option<DocFunction> {
    if has_doc_false(&f.annotation) {
        return None;
    }

    let params = f
        .params
        .iter()
        .map(|p| match p {
            Param::Self_ { .. } => DocParam {
                name: "self".to_string(),
                type_name: String::new(),
            },
            Param::Regular {
                name, type_expr, ..
            } => DocParam {
                name: name.clone(),
                type_name: type_expr_to_string(type_expr),
            },
        })
        .collect();

    Some(DocFunction {
        name: f.name.clone(),
        params,
        return_type: f.return_type.as_ref().map(type_expr_to_string),
        doc: annotation_string(&f.annotation),
    })
}

fn extract_struct(s: &StructDecl) -> Option<DocStruct> {
    if has_doc_false(&s.annotation) {
        return None;
    }

    let fields = s
        .fields
        .iter()
        .map(|f| DocField {
            name: f.name.clone(),
            type_name: type_expr_to_string(&f.type_expr),
        })
        .collect();

    Some(DocStruct {
        name: s.name.clone(),
        fields,
        doc: annotation_string(&s.annotation),
        functions: Vec::new(),
    })
}

fn extract_enum(e: &EnumDecl) -> Option<DocEnum> {
    if has_doc_false(&e.annotation) {
        return None;
    }

    let variants = e.variants.iter().map(|v| v.name.clone()).collect();

    Some(DocEnum {
        name: e.name.clone(),
        variants,
        doc: annotation_string(&e.annotation),
    })
}

fn extract_constant(c: &expo_ast::ast::Constant) -> Option<DocConstant> {
    if has_doc_false(&c.annotation) {
        return None;
    }

    Some(DocConstant {
        name: c.name.clone(),
        doc: annotation_string(&c.annotation),
    })
}

fn attach_impl_functions(imp: &ImplBlock, structs: &mut [DocStruct]) {
    let target_name = match &imp.target {
        TypeExpr::Named { path, .. } => path.last().cloned().unwrap_or_default(),
        _ => return,
    };

    for member in &imp.members {
        if let ImplMember::Function(f) = member {
            if f.is_private {
                continue;
            }
            if let Some(df) = extract_function(f)
                && let Some(ds) = structs.iter_mut().find(|s| s.name == target_name)
            {
                ds.functions.push(df);
            }
        }
    }
}

/// Format a type expression as a human-readable string.
fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, .. } => path.join("."),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_to_string).collect();
            format!("{}<{}>", path.join("."), args_str.join(", "))
        }
        TypeExpr::Ref { inner, .. } => format!("ref<{}>", type_expr_to_string(inner)),
        TypeExpr::Tuple { elements, .. } => {
            let elems: Vec<String> = elements.iter().map(type_expr_to_string).collect();
            format!("({})", elems.join(", "))
        }
        TypeExpr::Unit { .. } => "()".to_string(),
    }
}
