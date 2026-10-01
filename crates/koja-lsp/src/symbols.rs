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
    BuiltinDecl, Constant, EnumDecl, File, Function, ImplMember, Item, Param, ProtocolDecl,
    StructDecl, TestDecl, TypeExpr, TypeParam, Visibility, path_text,
};
use koja_ast::labels::type_expr_span;
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

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
    let Some(uri) = file.path.as_deref().and_then(path_to_uri) else {
        return;
    };
    let mut collector = WorkspaceSymbols {
        containers: Vec::new(),
        query,
        results,
        uri,
    };
    collector.visit_file(file);
}

/// Flat symbol collection for `workspace/symbol`. `containers` is
/// the stack of enclosing type names, and the innermost one is the
/// container a nested symbol reports.
struct WorkspaceSymbols<'a> {
    containers: Vec<String>,
    query: &'a str,
    results: &'a mut Vec<SymbolInformation>,
    uri: Uri,
}

impl WorkspaceSymbols<'_> {
    fn record(&mut self, name: &str, kind: SymbolKind, span: &Span) {
        if !self.query.is_empty() && !name.to_ascii_lowercase().contains(self.query) {
            return;
        }
        let container = self.containers.last().cloned();
        self.results
            .push(symbol_info(name, kind, &self.uri, span, container));
    }

    /// Run `walk` with `name` as the innermost container.
    fn in_container(&mut self, name: String, walk: impl FnOnce(&mut Self)) {
        self.containers.push(name);
        walk(self);
        self.containers.pop();
    }
}

impl<'ast> Visitor<'ast> for WorkspaceSymbols<'_> {
    /// A derived `impl` has no source to jump to.
    fn visit_item(&mut self, item: &'ast Item) {
        if let Item::Impl(block) = item
            && block.span.synthetic
        {
            return;
        }
        if let Some((name, kind, span)) = item_symbol(item) {
            self.record(name, kind, span);
        }
        match item_container(item) {
            Some(name) => self.in_container(name, |w| visit::walk_item(w, item)),
            None => visit::walk_item(self, item),
        }
    }

    /// A body declares no symbols, so the walk stops here.
    fn visit_function(&mut self, function: &'ast Function) {
        let kind = if self.containers.is_empty() {
            SymbolKind::FUNCTION
        } else {
            SymbolKind::METHOD
        };
        self.record(function.name.as_str(), kind, &function.span);
    }

    fn visit_test(&mut self, test: &'ast TestDecl) {
        self.record(&test.description, SymbolKind::EVENT, &test.span);
    }
}

/// The symbol an item declares under its own name. Functions and
/// tests have hooks of their own. `impl` and `extend` blocks name a
/// container, not a symbol.
fn item_symbol(item: &Item) -> Option<(&str, SymbolKind, &Span)> {
    match item {
        Item::Alias(_) | Item::Extend(_) | Item::Function(_) | Item::Impl(_) | Item::Test(_) => {
            None
        }
        Item::Builtin(decl) => Some((decl.name().as_str(), SymbolKind::STRUCT, &decl.span)),
        Item::Constant(constant) => Some((
            constant.name().as_str(),
            SymbolKind::CONSTANT,
            &constant.span,
        )),
        Item::Enum(decl) => Some((decl.name().as_str(), SymbolKind::ENUM, &decl.span)),
        Item::Protocol(decl) => Some((decl.name().as_str(), SymbolKind::INTERFACE, &decl.span)),
        Item::Struct(decl) => Some((decl.name().as_str(), SymbolKind::STRUCT, &decl.span)),
        Item::TypeAlias(alias) => {
            Some((alias.name.as_str(), SymbolKind::TYPE_PARAMETER, &alias.span))
        }
    }
}

/// The container name the item's members report, when it has
/// members.
fn item_container(item: &Item) -> Option<String> {
    match item {
        Item::Builtin(decl) => Some(decl.name().text.clone()),
        Item::Enum(decl) => Some(decl.name().text.clone()),
        Item::Extend(block) => Some(type_expr_label(&block.target)),
        Item::Impl(block) => Some(type_expr_label(&block.target)),
        Item::Struct(decl) => Some(decl.name().text.clone()),
        _ => None,
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
            Item::Constant(c) => symbols.push(constant_symbol(c)),
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
            Item::Protocol(p) => symbols.push(protocol_symbol(p)),
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

fn constant_symbol(c: &Constant) -> DocumentSymbol {
    #[allow(deprecated)]
    DocumentSymbol {
        name: c.name().text.clone(),
        detail: detail_with_visibility(c.visibility, None),
        kind: SymbolKind::CONSTANT,
        tags: None,
        deprecated: None,
        range: span_to_range(&c.span),
        selection_range: span_to_range(&c.name().span),
        children: None,
    }
}

fn builtin_symbol(b: &BuiltinDecl) -> DocumentSymbol {
    let mut children = nested_symbols(&b.nested);
    children.extend(b.functions.iter().map(function_symbol));
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

fn protocol_symbol(p: &ProtocolDecl) -> DocumentSymbol {
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
    DocumentSymbol {
        name: p.name().text.clone(),
        detail: detail_with_visibility(p.visibility, type_params_detail(&p.type_params)),
        kind: SymbolKind::INTERFACE,
        tags: None,
        deprecated: None,
        range: span_to_range(&p.span),
        selection_range: span_to_range(&p.name().span),
        children: children_option(children),
    }
}

fn nested_symbols(nested: &[Item]) -> Vec<DocumentSymbol> {
    nested
        .iter()
        .filter_map(|item| match item {
            Item::Constant(c) => Some(constant_symbol(c)),
            Item::Enum(e) => Some(enum_symbol(e)),
            Item::Protocol(p) => Some(protocol_symbol(p)),
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
