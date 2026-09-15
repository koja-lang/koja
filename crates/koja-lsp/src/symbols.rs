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
    BuiltinDecl, EnumDecl, File, Function, ImplMember, Item, Param, StructDecl, TestDecl, TypeExpr,
    TypeParam, Visibility, path_text,
};
use koja_ast::labels::type_expr_span;
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
        TypeExpr::Named { path, .. } => path_text(path),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_label).collect();
            format!("{}<{}>", path_text(path), args_str.join(", "))
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
            Item::Builtin(b) => {
                let name = b.name().as_str();
                if matches(name) {
                    results.push(symbol_info(name, SymbolKind::STRUCT, &uri, &b.span, None));
                }
                for f in &b.functions {
                    if matches(f.name.as_str()) {
                        results.push(symbol_info(
                            f.name.as_str(),
                            SymbolKind::METHOD,
                            &uri,
                            &f.span,
                            Some(name.to_string()),
                        ));
                    }
                }
                for t in &b.tests {
                    if matches(&t.description) {
                        results.push(symbol_info(
                            &t.description,
                            SymbolKind::EVENT,
                            &uri,
                            &t.span,
                            Some(name.to_string()),
                        ));
                    }
                }
            }
            Item::Function(f) => {
                if matches(f.name.as_str()) {
                    results.push(symbol_info(
                        f.name.as_str(),
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
            Item::Test(t) => {
                if matches(&t.description) {
                    results.push(symbol_info(
                        &t.description,
                        SymbolKind::EVENT,
                        &uri,
                        &t.span,
                        None,
                    ));
                }
            }
            Item::Constant(c) => {
                if matches(c.name.as_str()) {
                    results.push(symbol_info(
                        c.name.as_str(),
                        SymbolKind::CONSTANT,
                        &uri,
                        &c.span,
                        None,
                    ));
                }
            }
            Item::Protocol(p) => {
                if matches(p.name.as_str()) {
                    results.push(symbol_info(
                        p.name.as_str(),
                        SymbolKind::INTERFACE,
                        &uri,
                        &p.span,
                        None,
                    ));
                }
            }
            Item::TypeAlias(t) => {
                if matches(t.name.as_str()) {
                    results.push(symbol_info(
                        t.name.as_str(),
                        SymbolKind::TYPE_PARAMETER,
                        &uri,
                        &t.span,
                        None,
                    ));
                }
            }
            Item::Impl(imp) => {
                if imp.span.synthetic {
                    continue;
                }
                collect_member_workspace_symbols(
                    &imp.members,
                    &imp.tests,
                    &type_expr_label(&imp.target),
                    &uri,
                    query,
                    results,
                );
            }
            Item::Extend(ext) => {
                collect_member_workspace_symbols(
                    &ext.members,
                    &ext.tests,
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
    let (name, kind, span, functions, nested, tests) = match item {
        Item::Enum(e) => (
            e.name().as_str(),
            SymbolKind::ENUM,
            &e.span,
            &e.functions,
            &e.nested,
            &e.tests[..],
        ),
        Item::Struct(s) => (
            s.name().as_str(),
            SymbolKind::STRUCT,
            &s.span,
            &s.functions,
            &s.nested,
            &s.tests[..],
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
        if matches(f.name.as_str()) {
            results.push(symbol_info(
                f.name.as_str(),
                SymbolKind::METHOD,
                uri,
                &f.span,
                Some(name.to_string()),
            ));
        }
    }
    for t in tests {
        if matches(&t.description) {
            results.push(symbol_info(
                &t.description,
                SymbolKind::EVENT,
                uri,
                &t.span,
                Some(name.to_string()),
            ));
        }
    }
    for nested_item in nested {
        collect_type_workspace_symbols(nested_item, Some(name), uri, query, results);
    }
}

/// Collects the function members and tests of an `impl`/`extend` block.
fn collect_member_workspace_symbols(
    members: &[ImplMember],
    tests: &[TestDecl],
    container: &str,
    uri: &Uri,
    query: &str,
    results: &mut Vec<SymbolInformation>,
) {
    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(query);
    for member in members {
        if let ImplMember::Function(f) = member
            && matches(f.name.as_str())
        {
            results.push(symbol_info(
                f.name.as_str(),
                SymbolKind::METHOD,
                uri,
                &f.span,
                Some(container.to_string()),
            ));
        }
    }
    for t in tests {
        if matches(&t.description) {
            results.push(symbol_info(
                &t.description,
                SymbolKind::EVENT,
                uri,
                &t.span,
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
            Item::Builtin(b) => symbols.push(builtin_symbol(b)),
            Item::Function(f) => symbols.push(function_symbol(f)),
            Item::Struct(s) => symbols.push(struct_symbol(s)),
            Item::Test(t) => symbols.push(test_symbol(t)),
            Item::Enum(e) => symbols.push(enum_symbol(e)),
            Item::Constant(c) => {
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: c.name.text.clone(),
                    detail: detail_with_visibility(c.visibility, None),
                    kind: SymbolKind::CONSTANT,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(&c.span),
                    selection_range: span_to_range(&c.name.span),
                    children: None,
                });
            }
            Item::Impl(imp) => {
                if imp.span.synthetic {
                    continue;
                }
                let target_name = type_expr_label(&imp.target);
                let children = member_symbols(&imp.members, &imp.tests);

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: target_name,
                    detail: Some(format!("impl {}", type_expr_label(&imp.trait_expr))),
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(&imp.span),
                    selection_range: span_to_range(&type_expr_span(&imp.target)),
                    children: children_option(children),
                });
            }
            Item::Extend(ext) => {
                let target_name = type_expr_label(&ext.target);
                let children = member_symbols(&ext.members, &ext.tests);

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: target_name,
                    detail: Some("extend".to_string()),
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(&ext.span),
                    selection_range: span_to_range(&type_expr_span(&ext.target)),
                    children: children_option(children),
                });
            }
            Item::Protocol(p) => {
                let children: Vec<DocumentSymbol> = p
                    .methods
                    .iter()
                    .map(|m| {
                        #[allow(deprecated)]
                        DocumentSymbol {
                            name: m.name.text.clone(),
                            detail: None,
                            kind: SymbolKind::METHOD,
                            tags: None,
                            deprecated: None,
                            range: span_to_range(&m.span),
                            selection_range: span_to_range(&m.name.span),
                            children: None,
                        }
                    })
                    .collect();

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: p.name.text.clone(),
                    detail: detail_with_visibility(
                        p.visibility,
                        type_params_detail(&p.type_params),
                    ),
                    kind: SymbolKind::INTERFACE,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(&p.span),
                    selection_range: span_to_range(&p.name.span),
                    children: children_option(children),
                });
            }
            Item::TypeAlias(ta) => {
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: ta.name.text.clone(),
                    detail: detail_with_visibility(
                        ta.visibility,
                        Some(type_expr_label(&ta.type_expr)),
                    ),
                    kind: SymbolKind::TYPE_PARAMETER,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(&ta.span),
                    selection_range: span_to_range(&ta.name.span),
                    children: None,
                });
            }
        }
    }

    symbols
}

/// Builds a [`DocumentSymbol`] for a builtin declaration.
/// Children of an `impl`/`extend` block: its methods, then its tests.
fn member_symbols(members: &[ImplMember], tests: &[TestDecl]) -> Vec<DocumentSymbol> {
    members
        .iter()
        .filter_map(|m| match m {
            ImplMember::Function(f) => Some(function_symbol(f)),
            _ => None,
        })
        .chain(tests.iter().map(test_symbol))
        .collect()
}

fn builtin_symbol(b: &BuiltinDecl) -> DocumentSymbol {
    let mut children: Vec<DocumentSymbol> = b.functions.iter().map(function_symbol).collect();
    children.extend(b.tests.iter().map(test_symbol));
    #[allow(deprecated)]
    DocumentSymbol {
        name: b.name().text.clone(),
        detail: type_params_detail(&b.type_params),
        kind: SymbolKind::STRUCT,
        tags: None,
        deprecated: None,
        range: span_to_range(&b.span),
        selection_range: span_to_range(&b.name().span),
        children: children_option(children),
    }
}

/// Builds a [`DocumentSymbol`] for a struct declaration.
fn struct_symbol(s: &StructDecl) -> DocumentSymbol {
    let mut children = nested_symbols(&s.nested);
    children.extend(s.functions.iter().map(function_symbol));
    children.extend(s.tests.iter().map(test_symbol));
    #[allow(deprecated)]
    DocumentSymbol {
        name: s.name().text.clone(),
        detail: detail_with_visibility(s.visibility, type_params_detail(&s.type_params)),
        kind: SymbolKind::STRUCT,
        tags: None,
        deprecated: None,
        range: span_to_range(&s.span),
        selection_range: span_to_range(&s.name().span),
        children: children_option(children),
    }
}

/// The description is the name, since a test has no identifier of
/// its own.
fn test_symbol(t: &TestDecl) -> DocumentSymbol {
    let range = span_to_range(&t.span);
    #[allow(deprecated)]
    DocumentSymbol {
        name: t.description.clone(),
        detail: Some("test".to_string()),
        kind: SymbolKind::EVENT,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    }
}

/// Builds a [`DocumentSymbol`] for an enum declaration.
fn enum_symbol(e: &EnumDecl) -> DocumentSymbol {
    let mut children: Vec<DocumentSymbol> = e
        .variants
        .iter()
        .map(|v| {
            let vrange = span_to_range(&v.span);
            #[allow(deprecated)]
            DocumentSymbol {
                name: v.name.text.clone(),
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
    children.extend(e.tests.iter().map(test_symbol));
    #[allow(deprecated)]
    DocumentSymbol {
        name: e.name().text.clone(),
        detail: detail_with_visibility(e.visibility, type_params_detail(&e.type_params)),
        kind: SymbolKind::ENUM,
        tags: None,
        deprecated: None,
        range: span_to_range(&e.span),
        selection_range: span_to_range(&e.name().span),
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
        name: f.name.text.clone(),
        detail: detail_with_visibility(f.visibility, Some(detail)),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        range: span_to_range(&f.span),
        selection_range: span_to_range(&f.name.span),
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
