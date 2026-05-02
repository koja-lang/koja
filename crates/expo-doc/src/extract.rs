//! Walk the parsed AST and extract documentation items.

use expo_ast::ast::{
    AnnotationValue, EnumDecl, Function, ImplBlock, ImplMember, Item, Module, Param, ProtocolDecl,
    ProtocolMethod, StructDecl, TypeExpr, Visibility,
};

/// Summary of a documentable item for the flat index listing.
#[derive(Debug)]
pub struct DocItem {
    pub doc: Option<String>,
    pub kind: String,
    pub name: String,
}

/// Documentation for a constant.
#[derive(Debug)]
pub struct DocConstant {
    pub doc: Option<String>,
    pub name: String,
}

/// Documentation for an enum.
#[derive(Debug)]
pub struct DocEnum {
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub variants: Vec<String>,
}

/// A struct field for display.
#[derive(Debug)]
pub struct DocField {
    pub name: String,
    pub type_name: String,
}

/// Documentation for a function.
#[derive(Debug)]
pub struct DocFunction {
    pub doc: Option<String>,
    pub name: String,
    pub params: Vec<DocParam>,
    pub return_type: Option<String>,
    pub type_params: Vec<String>,
}

/// A function parameter for display.
#[derive(Debug)]
pub struct DocParam {
    pub name: String,
    pub type_name: String,
}

/// Documentation for a protocol.
#[derive(Debug)]
pub struct DocProtocol {
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// Documentation for a struct, including its impl functions.
#[derive(Debug)]
pub struct DocStruct {
    pub doc: Option<String>,
    pub fields: Vec<DocField>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// All extracted documentation from a project, flattened into a single namespace.
#[derive(Debug)]
pub struct DocProject {
    pub constants: Vec<DocConstant>,
    pub enums: Vec<DocEnum>,
    pub functions: Vec<DocFunction>,
    pub items: Vec<DocItem>,
    pub protocols: Vec<DocProtocol>,
    pub structs: Vec<DocStruct>,
}

/// Extract documentation items from a parsed file into the running project.
///
/// Items with `@doc false` are excluded.
pub fn extract_items(file: &Module, project: &mut DocProject) {
    let mut local_structs: Vec<DocStruct> = Vec::new();
    let mut local_enums: Vec<DocEnum> = Vec::new();

    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Constant(c) => {
                if let Some(dc) = extract_constant(c) {
                    project.constants.push(dc);
                }
            }
            Item::Enum(e) => {
                if let Some(de) = extract_enum(e) {
                    local_enums.push(de);
                }
            }
            Item::Function(f) => {
                if f.visibility == Visibility::Public
                    && let Some(df) = extract_function(f)
                {
                    project.functions.push(df);
                }
            }
            Item::Impl(imp) => {
                attach_impl_functions(
                    imp,
                    &mut local_structs,
                    &mut local_enums,
                    &mut project.structs,
                    &mut project.enums,
                );
            }
            Item::Protocol(p) => {
                if let Some(dp) = extract_protocol(p) {
                    project.protocols.push(dp);
                }
            }
            Item::Struct(s) => {
                if let Some(ds) = extract_struct(s) {
                    local_structs.push(ds);
                }
            }
            _ => {}
        }
    }

    project.structs.extend(local_structs);
    project.enums.extend(local_enums);
}

/// Finalize the project: sort everything and build the flat item index.
pub fn finalize_project(project: &mut DocProject) {
    project.constants.sort_by(|a, b| a.name.cmp(&b.name));
    project.enums.sort_by(|a, b| a.name.cmp(&b.name));
    project.functions.sort_by(|a, b| a.name.cmp(&b.name));
    project.protocols.sort_by(|a, b| a.name.cmp(&b.name));
    project.structs.sort_by(|a, b| a.name.cmp(&b.name));

    for e in &mut project.enums {
        e.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for p in &mut project.protocols {
        p.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for s in &mut project.structs {
        s.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }

    project.items.clear();
    for c in &project.constants {
        project.items.push(DocItem {
            doc: c.doc.clone(),
            kind: "const".to_string(),
            name: c.name.clone(),
        });
    }
    for e in &project.enums {
        project.items.push(DocItem {
            doc: e.doc.clone(),
            kind: "enum".to_string(),
            name: e.name.clone(),
        });
    }
    for f in &project.functions {
        project.items.push(DocItem {
            doc: f.doc.clone(),
            kind: "fn".to_string(),
            name: f.name.clone(),
        });
    }
    for p in &project.protocols {
        project.items.push(DocItem {
            doc: p.doc.clone(),
            kind: "protocol".to_string(),
            name: p.name.clone(),
        });
    }
    for s in &project.structs {
        project.items.push(DocItem {
            doc: s.doc.clone(),
            kind: "struct".to_string(),
            name: s.name.clone(),
        });
    }
    project.items.sort_by(|a, b| a.name.cmp(&b.name));
}

fn annotation_string(annotations: &[expo_ast::ast::Annotation]) -> Option<String> {
    annotations
        .iter()
        .find(|a| a.name == "doc")
        .and_then(|a| match &a.value {
            Some(AnnotationValue::String(s)) => Some(s.clone()),
            _ => None,
        })
}

fn attach_impl_functions(
    imp: &ImplBlock,
    local_structs: &mut Vec<DocStruct>,
    local_enums: &mut [DocEnum],
    project_structs: &mut [DocStruct],
    project_enums: &mut [DocEnum],
) {
    if imp.trait_expr.is_some() {
        return;
    }

    let target_name = match &imp.target {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => {
            path.last().cloned().unwrap_or_default()
        }
        _ => return,
    };

    let mut funcs = Vec::new();
    for member in &imp.members {
        if let ImplMember::Function(f) = member {
            if f.visibility == Visibility::Private {
                continue;
            }
            if let Some(df) = extract_function(f) {
                funcs.push(df);
            }
        }
    }

    if funcs.is_empty() {
        return;
    }

    if let Some(ds) = local_structs.iter_mut().find(|s| s.name == target_name) {
        ds.functions.extend(funcs);
    } else if let Some(ds) = project_structs.iter_mut().find(|s| s.name == target_name) {
        ds.functions.extend(funcs);
    } else if let Some(de) = local_enums.iter_mut().find(|e| e.name == target_name) {
        de.functions.extend(funcs);
    } else if let Some(de) = project_enums.iter_mut().find(|e| e.name == target_name) {
        de.functions.extend(funcs);
    } else {
        local_structs.push(DocStruct {
            doc: None,
            fields: Vec::new(),
            functions: funcs,
            name: target_name,
            type_params: Vec::new(),
        });
    }
}

fn extract_constant(c: &expo_ast::ast::Constant) -> Option<DocConstant> {
    if has_doc_false(&c.annotations) {
        return None;
    }

    Some(DocConstant {
        doc: annotation_string(&c.annotations),
        name: c.name.clone(),
    })
}

fn extract_enum(e: &EnumDecl) -> Option<DocEnum> {
    if has_doc_false(&e.annotations) {
        return None;
    }

    let variants = e.variants.iter().map(|v| v.name.clone()).collect();
    let functions = e.functions.iter().filter_map(extract_function).collect();

    Some(DocEnum {
        doc: annotation_string(&e.annotations),
        functions,
        name: e.name.clone(),
        variants,
    })
}

fn extract_function(f: &Function) -> Option<DocFunction> {
    if has_doc_false(&f.annotations) {
        return None;
    }

    let params = extract_params(&f.params);

    Some(DocFunction {
        doc: annotation_string(&f.annotations),
        name: f.name.clone(),
        params,
        return_type: f.return_type.as_ref().map(type_expr_to_string),
        type_params: f.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn extract_params(params: &[Param]) -> Vec<DocParam> {
    params
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
        .collect()
}

fn extract_protocol(p: &ProtocolDecl) -> Option<DocProtocol> {
    if has_doc_false(&p.annotations) {
        return None;
    }

    let functions = p
        .methods
        .iter()
        .filter_map(extract_protocol_method)
        .collect();

    Some(DocProtocol {
        doc: annotation_string(&p.annotations),
        functions,
        name: p.name.clone(),
        type_params: p.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn extract_protocol_method(m: &ProtocolMethod) -> Option<DocFunction> {
    if has_doc_false(&m.annotations) {
        return None;
    }

    let params = extract_params(&m.params);

    Some(DocFunction {
        doc: annotation_string(&m.annotations),
        name: m.name.clone(),
        params,
        return_type: m.return_type.as_ref().map(type_expr_to_string),
        type_params: m.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn extract_struct(s: &StructDecl) -> Option<DocStruct> {
    if has_doc_false(&s.annotations) {
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
    let functions = s.functions.iter().filter_map(extract_function).collect();

    Some(DocStruct {
        doc: annotation_string(&s.annotations),
        fields,
        functions,
        name: s.name.clone(),
        type_params: s.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn has_doc_false(annotations: &[expo_ast::ast::Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| a.name == "doc" && a.value == Some(AnnotationValue::False))
}

/// Format a type expression as a human-readable string.
fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, .. } => path.join("."),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_to_string).collect();
            format!("{}<{}>", path.join("."), args_str.join(", "))
        }
        TypeExpr::Unit { .. } => "()".to_string(),
        TypeExpr::Self_ { .. } => "Self".to_string(),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(type_expr_to_string).collect();
            format!(
                "fn({}) -> {}",
                ps.join(", "),
                type_expr_to_string(return_type)
            )
        }
        TypeExpr::Union { types, .. } => {
            let parts: Vec<String> = types.iter().map(type_expr_to_string).collect();
            parts.join(" | ")
        }
    }
}
