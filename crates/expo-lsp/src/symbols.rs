//! Symbol providers for the Expo LSP.
//!
//! **Document symbols** (`textDocument/documentSymbol`): maps the parsed AST
//! of an open document into a hierarchical list of [`DocumentSymbol`]s,
//! powering the editor's outline view, breadcrumbs, and `Cmd+Shift+O`.
//!
//! **Workspace symbols** (`workspace/symbol`): searches all project modules
//! for symbols matching a query string, powering `Cmd+T` / `#` search.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{
    Function, ImplMember, Item, Module, Param, PassMode, TypeExpr, TypeParam, Visibility,
};

use crate::backend::Backend;
use crate::convert::{path_to_uri, span_to_range};

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
            param_modes,
            return_type,
            ..
        } => {
            let ps: Vec<String> = params
                .iter()
                .zip(param_modes.iter())
                .map(|(p, mode)| {
                    let label = type_expr_label(p);
                    if *mode == PassMode::Move {
                        format!("move {label}")
                    } else {
                        label
                    }
                })
                .collect();
            format!("fn ({}) -> {}", ps.join(", "), type_expr_label(return_type))
        }
        TypeExpr::Self_ { .. } => "Self".to_string(),
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

        let symbols = build_document_symbols(&state.module);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    /// Handles `workspace/symbol` requests by searching all project modules
    /// and open documents for symbols matching the query.
    pub(crate) async fn handle_workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let query = params.query.to_ascii_lowercase();
        let mut results = Vec::new();

        let project_mods = self.project_modules.read().await;
        for module in project_mods.iter() {
            collect_workspace_symbols(module, &query, &mut results);
        }

        let docs = self.documents.read().await;
        for state in docs.values() {
            collect_workspace_symbols(&state.module, &query, &mut results);
        }

        Ok(Some(WorkspaceSymbolResponse::Flat(results)))
    }
}

/// Collects workspace symbols from a module, filtering by query substring.
#[allow(deprecated)]
fn collect_workspace_symbols(module: &Module, query: &str, results: &mut Vec<SymbolInformation>) {
    let uri = module.path.as_deref().and_then(path_to_uri);
    let uri = match uri {
        Some(u) => u,
        None => return,
    };

    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(query);

    for item in &module.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => {
                if matches(&f.name) {
                    results.push(SymbolInformation {
                        name: f.name.clone(),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&f.span),
                        },
                        container_name: None,
                    });
                }
            }
            Item::Struct(s) => {
                if matches(&s.name) {
                    results.push(SymbolInformation {
                        name: s.name.clone(),
                        kind: SymbolKind::STRUCT,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&s.span),
                        },
                        container_name: None,
                    });
                }
                for f in &s.functions {
                    if matches(&f.name) {
                        results.push(SymbolInformation {
                            name: f.name.clone(),
                            kind: SymbolKind::METHOD,
                            tags: None,
                            deprecated: None,
                            location: Location {
                                uri: uri.clone(),
                                range: span_to_range(&f.span),
                            },
                            container_name: Some(s.name.clone()),
                        });
                    }
                }
            }
            Item::Enum(e) => {
                if matches(&e.name) {
                    results.push(SymbolInformation {
                        name: e.name.clone(),
                        kind: SymbolKind::ENUM,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&e.span),
                        },
                        container_name: None,
                    });
                }
                for f in &e.functions {
                    if matches(&f.name) {
                        results.push(SymbolInformation {
                            name: f.name.clone(),
                            kind: SymbolKind::METHOD,
                            tags: None,
                            deprecated: None,
                            location: Location {
                                uri: uri.clone(),
                                range: span_to_range(&f.span),
                            },
                            container_name: Some(e.name.clone()),
                        });
                    }
                }
            }
            Item::Constant(c) => {
                if matches(&c.name) {
                    results.push(SymbolInformation {
                        name: c.name.clone(),
                        kind: SymbolKind::CONSTANT,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&c.span),
                        },
                        container_name: None,
                    });
                }
            }
            Item::Protocol(p) => {
                if matches(&p.name) {
                    results.push(SymbolInformation {
                        name: p.name.clone(),
                        kind: SymbolKind::INTERFACE,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&p.span),
                        },
                        container_name: None,
                    });
                }
            }
            Item::TypeAlias(t) => {
                if matches(&t.name) {
                    results.push(SymbolInformation {
                        name: t.name.clone(),
                        kind: SymbolKind::TYPE_PARAMETER,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: span_to_range(&t.span),
                        },
                        container_name: None,
                    });
                }
            }
            Item::Impl(imp) => {
                let container = type_expr_label(&imp.target);
                for member in &imp.members {
                    if let ImplMember::Function(f) = member
                        && matches(&f.name)
                    {
                        results.push(SymbolInformation {
                            name: f.name.clone(),
                            kind: SymbolKind::METHOD,
                            tags: None,
                            deprecated: None,
                            location: Location {
                                uri: uri.clone(),
                                range: span_to_range(&f.span),
                            },
                            container_name: Some(container.clone()),
                        });
                    }
                }
            }
            Item::Shared(_) => {}
        }
    }
}

/// Converts a parsed module's top-level items into document symbols.
fn build_document_symbols(module: &Module) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();

    for item in &module.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => symbols.push(function_symbol(f)),
            Item::Struct(s) => {
                let range = span_to_range(&s.span);
                let children: Vec<DocumentSymbol> =
                    s.functions.iter().map(function_symbol).collect();
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: s.name.clone(),
                    detail: type_params_detail(&s.type_params),
                    kind: SymbolKind::STRUCT,
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
            Item::Enum(e) => {
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
                children.extend(e.functions.iter().map(function_symbol));

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: e.name.clone(),
                    detail: type_params_detail(&e.type_params),
                    kind: SymbolKind::ENUM,
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
            Item::Constant(c) => {
                let range = span_to_range(&c.span);
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: c.name.clone(),
                    detail: None,
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
                    detail: imp
                        .trait_expr
                        .as_ref()
                        .map(|t| format!("impl {}", type_expr_label(t))),
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
                    detail: type_params_detail(&p.type_params),
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
                    detail: Some(type_expr_label(&ta.type_expr)),
                    kind: SymbolKind::TYPE_PARAMETER,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range: range,
                    children: None,
                });
            }
            Item::Shared(_) => {}
        }
    }

    symbols
}

/// Builds a [`DocumentSymbol`] for a function declaration.
fn function_symbol(f: &Function) -> DocumentSymbol {
    let range = span_to_range(&f.span);
    let vis = if f.visibility == Visibility::Private {
        "priv "
    } else {
        ""
    };
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
    let detail = format!("{}fn({}){}", vis, params.join(", "), ret);

    #[allow(deprecated)]
    DocumentSymbol {
        name: f.name.clone(),
        detail: Some(detail),
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
