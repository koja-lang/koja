//! Symbol providers for the Koja LSP.
//!
//! **Document symbols** (`textDocument/documentSymbol`): maps the parsed AST
//! of an open document into a hierarchical list of [`DocumentSymbol`]s,
//! powering the editor's outline view, breadcrumbs, and `Cmd+Shift+O`.
//!
//! **Workspace symbols** (`workspace/symbol`): searches all project files
//! for symbols matching a query string, powering `Cmd+T` / `#` search.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::{
    EnumDecl, File, Function, ImplMember, Item, Param, StructDecl, TypeExpr, TypeParam, Visibility,
};
use koja_ast::span::Span;

use crate::backend::Backend;
use crate::convert::{path_to_uri, span_to_range};

/// Prefixes `detail` with `priv` for private declarations.
fn detail_with_visibility(visibility: Visibility, detail: Option<String>) -> Option<String> {
    if visibility == Visibility::Public {
        return detail;
    }
    match detail {
        Some(d) => Some(format!("priv {d}")),
        None => Some("priv".to_string()),
    }
}

/// Formats a [`TypeExpr`] into a human-readable string for symbol details.
fn type_expr_label(te: &TypeExpr) -> String {
    match te {
        TypeExpr::Named { path, .. } => path.join("."),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_label).collect();
            format!("{}<{}>", path.join("."), args_str.join(", "))
        }
        TypeExpr::Unit { .. } => "()".to_string(),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(type_expr_label).collect();
            format!("fn ({}) -> {}", ps.join(", "), type_expr_label(return_type))
        }
        TypeExpr::Self_ { .. } => "Self".to_string(),
        TypeExpr::Tuple { elements, .. } => {
            let es: Vec<String> = elements.iter().map(type_expr_label).collect();
            format!("({})", es.join(", "))
        }
        TypeExpr::Union { types, .. } => {
            let ts: Vec<String> = types.iter().map(type_expr_label).collect();
            ts.join(" | ")
        }
    }
}

impl Backend {
    /// Handles `textDocument/documentSymbol` requests by converting the
    /// cached AST into a hierarchy of LSP document symbols.
    pub(crate) async fn handle_document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let symbols = state
            .active_file()
            .map(build_document_symbols)
            .unwrap_or_default();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    /// Handles `workspace/symbol` requests by searching every open
    /// document (and its sibling project files) for symbols matching
    /// the query. There is no separate project-files cache. Sibling
    /// state lives in each document's `parsed` bundle, so we walk
    /// those instead.
    pub(crate) async fn handle_workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let query = params.query.to_ascii_lowercase();
        let mut results = Vec::new();

        let docs = self.documents.read().await;
        for state in docs.values() {
            for parsed_file in state.parsed.iter() {
                collect_workspace_symbols(&parsed_file.ast, &query, &mut results);
            }
        }

        Ok(Some(WorkspaceSymbolResponse::Flat(results)))
    }
}

/// Builds a flat workspace symbol entry.
#[allow(deprecated)]
fn symbol_info(
    name: &str,
    kind: SymbolKind,
    uri: &Uri,
    span: &Span,
    container: Option<String>,
) -> SymbolInformation {
    SymbolInformation {
        name: name.to_string(),
        kind,
        tags: None,
        deprecated: None,
        location: Location {
            uri: uri.clone(),
            range: span_to_range(span),
        },
        container_name: container,
    }
}

/// Collects workspace symbols from a file, filtering by query substring.
fn collect_workspace_symbols(file: &File, query: &str, results: &mut Vec<SymbolInformation>) {
    let uri = file.path.as_deref().and_then(path_to_uri);
    let uri = match uri {
        Some(u) => u,
        None => return,
    };

    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(query);

    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => {
                if matches(&f.name) {
                    results.push(symbol_info(
                        &f.name,
                        SymbolKind::FUNCTION,
                        &uri,
                        &f.span,
                        None,
                    ));
                }
            }
            Item::Struct(_) | Item::Enum(_) => {
                collect_type_workspace_symbols(item, None, &uri, query, results);
            }
            Item::Constant(c) => {
                if matches(&c.name) {
                    results.push(symbol_info(
                        &c.name,
                        SymbolKind::CONSTANT,
                        &uri,
                        &c.span,
                        None,
                    ));
                }
            }
            Item::Protocol(p) => {
                if matches(&p.name) {
                    results.push(symbol_info(
                        &p.name,
                        SymbolKind::INTERFACE,
                        &uri,
                        &p.span,
                        None,
                    ));
                }
            }
            Item::TypeAlias(t) => {
                if matches(&t.name) {
                    results.push(symbol_info(
                        &t.name,
                        SymbolKind::TYPE_PARAMETER,
                        &uri,
                        &t.span,
                        None,
                    ));
                }
            }
            Item::Impl(imp) => {
                collect_member_workspace_symbols(
                    &imp.members,
                    &type_expr_label(&imp.target),
                    &uri,
                    query,
                    results,
                );
            }
            Item::Extend(ext) => {
                collect_member_workspace_symbols(
                    &ext.members,
                    &type_expr_label(&ext.target),
                    &uri,
                    query,
                    results,
                );
            }
        }
    }
}

/// Collects a struct/enum, its functions, and its nested types.
fn collect_type_workspace_symbols(
    item: &Item,
    container: Option<&str>,
    uri: &Uri,
    query: &str,
    results: &mut Vec<SymbolInformation>,
) {
    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(query);
    let (name, kind, span, functions, nested) = match item {
        Item::Enum(e) => (e.name(), SymbolKind::ENUM, &e.span, &e.functions, &e.nested),
        Item::Struct(s) => (
            s.name(),
            SymbolKind::STRUCT,
            &s.span,
            &s.functions,
            &s.nested,
        ),
        _ => return,
    };
    if matches(name) {
        results.push(symbol_info(
            name,
            kind,
            uri,
            span,
            container.map(str::to_string),
        ));
    }
    for f in functions {
        if matches(&f.name) {
            results.push(symbol_info(
                &f.name,
                SymbolKind::METHOD,
                uri,
                &f.span,
                Some(name.to_string()),
            ));
        }
    }
    for nested_item in nested {
        collect_type_workspace_symbols(nested_item, Some(name), uri, query, results);
    }
}

/// Collects the function members of an `impl`/`extend` block.
fn collect_member_workspace_symbols(
    members: &[ImplMember],
    container: &str,
    uri: &Uri,
    query: &str,
    results: &mut Vec<SymbolInformation>,
) {
    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(query);
    for member in members {
        if let ImplMember::Function(f) = member
            && matches(&f.name)
        {
            results.push(symbol_info(
                &f.name,
                SymbolKind::METHOD,
                uri,
                &f.span,
                Some(container.to_string()),
            ));
        }
    }
}

/// Converts a parsed file's top-level items into document symbols.
fn build_document_symbols(file: &File) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();

    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => symbols.push(function_symbol(f)),
            Item::Struct(s) => symbols.push(struct_symbol(s)),
            Item::Enum(e) => symbols.push(enum_symbol(e)),
            Item::Constant(c) => {
                let range = span_to_range(&c.span);
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: c.name.clone(),
                    detail: detail_with_visibility(c.visibility, None),
                    kind: SymbolKind::CONSTANT,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Item::Impl(imp) => {
                let range = span_to_range(&imp.span);
                let target_name = type_expr_label(&imp.target);
                let children: Vec<DocumentSymbol> = imp
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        ImplMember::Function(f) => Some(function_symbol(f)),
                        _ => None,
                    })
                    .collect();

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: target_name,
                    detail: Some(format!("impl {}", type_expr_label(&imp.trait_expr))),
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Item::Extend(ext) => {
                let range = span_to_range(&ext.span);
                let target_name = type_expr_label(&ext.target);
                let children: Vec<DocumentSymbol> = ext
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        ImplMember::Function(f) => Some(function_symbol(f)),
                        _ => None,
                    })
                    .collect();

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: target_name,
                    detail: Some("extend".to_string()),
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Item::Protocol(p) => {
                let range = span_to_range(&p.span);
                let children: Vec<DocumentSymbol> = p
                    .methods
                    .iter()
                    .map(|m| {
                        let mrange = span_to_range(&m.span);
                        #[allow(deprecated)]
                        DocumentSymbol {
                            name: m.name.clone(),
                            detail: None,
                            kind: SymbolKind::METHOD,
                            tags: None,
                            deprecated: None,
                            range: mrange,
                            selection_range: mrange,
                            children: None,
                        }
                    })
                    .collect();

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: p.name.clone(),
                    detail: detail_with_visibility(
                        p.visibility,
                        type_params_detail(&p.type_params),
                    ),
                    kind: SymbolKind::INTERFACE,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: if children.is_empty() {
                        None
                    } else {
                        Some(children)
                    },
                });
            }
            Item::TypeAlias(ta) => {
                let range = span_to_range(&ta.span);
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: ta.name.clone(),
                    detail: detail_with_visibility(
                        ta.visibility,
                        Some(type_expr_label(&ta.type_expr)),
                    ),
                    kind: SymbolKind::TYPE_PARAMETER,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
        }
    }

    symbols
}

/// Builds a [`DocumentSymbol`] for a struct declaration.
fn struct_symbol(s: &StructDecl) -> DocumentSymbol {
    let range = span_to_range(&s.span);
    let mut children = nested_symbols(&s.nested);
    children.extend(s.functions.iter().map(function_symbol));
    #[allow(deprecated)]
    DocumentSymbol {
        name: s.name().to_string(),
        detail: detail_with_visibility(s.visibility, type_params_detail(&s.type_params)),
        kind: SymbolKind::STRUCT,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: children_option(children),
    }
}

/// Builds a [`DocumentSymbol`] for an enum declaration.
fn enum_symbol(e: &EnumDecl) -> DocumentSymbol {
    let range = span_to_range(&e.span);
    let mut children: Vec<DocumentSymbol> = e
        .variants
        .iter()
        .map(|v| {
            let vrange = span_to_range(&v.span);
            #[allow(deprecated)]
            DocumentSymbol {
                name: v.name.clone(),
                detail: None,
                kind: SymbolKind::ENUM_MEMBER,
                tags: None,
                deprecated: None,
                range: vrange,
                selection_range: vrange,
                children: None,
            }
        })
        .collect();
    children.extend(nested_symbols(&e.nested));
    children.extend(e.functions.iter().map(function_symbol));
    #[allow(deprecated)]
    DocumentSymbol {
        name: e.name().to_string(),
        detail: detail_with_visibility(e.visibility, type_params_detail(&e.type_params)),
        kind: SymbolKind::ENUM,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: children_option(children),
    }
}

fn nested_symbols(nested: &[Item]) -> Vec<DocumentSymbol> {
    nested
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) => Some(enum_symbol(e)),
            Item::Struct(s) => Some(struct_symbol(s)),
            _ => None,
        })
        .collect()
}

fn children_option(children: Vec<DocumentSymbol>) -> Option<Vec<DocumentSymbol>> {
    if children.is_empty() {
        None
    } else {
        Some(children)
    }
}

/// Builds a [`DocumentSymbol`] for a function declaration.
fn function_symbol(f: &Function) -> DocumentSymbol {
    let range = span_to_range(&f.span);
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| match p {
            Param::Self_ { .. } => "self".to_string(),
            Param::Regular {
                name, type_expr, ..
            } => format!("{}: {}", name, type_expr_label(type_expr)),
        })
        .collect();
    let ret = f
        .return_type
        .as_ref()
        .map(|t| format!(" -> {}", type_expr_label(t)))
        .unwrap_or_default();
    let detail = format!("fn({}){}", params.join(", "), ret);

    #[allow(deprecated)]
    DocumentSymbol {
        name: f.name.clone(),
        detail: detail_with_visibility(f.visibility, Some(detail)),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    }
}

/// Formats type parameters as a detail string like `<T, U>`, or `None`
/// if the list is empty.
fn type_params_detail(params: &[TypeParam]) -> Option<String> {
    if params.is_empty() {
        None
    } else {
        Some(format!(
            "<{}>",
            params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}
