//! Symbol providers for the Koja LSP.
//!
//! Both map the declaration tree from [`koja_query::outline`] onto
//! protocol types. Document symbols (`textDocument/documentSymbol`)
//! keep the tree for the outline view and breadcrumbs. Workspace
//! symbols (`workspace/symbol`) flatten it, with the enclosing
//! declaration's name as the container, and filter by the query.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::File;
use koja_ast::span::FileId;
use koja_query::outline::{OutlineKind, OutlineNode, outline};

use crate::backend::Backend;
use crate::convert::{Positions, path_to_uri};

impl Backend {
    /// Handles `textDocument/documentSymbol`.
    pub(crate) async fn handle_document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        self.with_state(&uri, |state| {
            let symbols = state
                .active_file()
                .map(|file| document_symbols(&outline(file), &state.active_positions()))
                .unwrap_or_default();
            Ok(Some(DocumentSymbolResponse::Nested(symbols)))
        })
        .await
    }

    /// Handles `workspace/symbol` by searching every open document
    /// and its sibling project files. There is no separate
    /// project-files cache. Sibling state lives in each document's
    /// `parsed` bundle, so we walk those instead.
    pub(crate) async fn handle_workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let query = params.query.to_ascii_lowercase();
        let mut results = Vec::new();

        let docs = self.documents.read().await;
        for state in docs.values() {
            // `parsed` iterates in file id order.
            for (index, parsed_file) in state.parsed.iter().enumerate() {
                let positions = state.positions(FileId(index as u32));
                workspace_symbols(&parsed_file.ast, &query, &positions, &mut results);
            }
        }

        Ok(Some(WorkspaceSymbolResponse::Flat(results)))
    }
}

fn symbol_kind(kind: OutlineKind) -> SymbolKind {
    match kind {
        OutlineKind::Builtin | OutlineKind::Struct => SymbolKind::STRUCT,
        OutlineKind::Constant => SymbolKind::CONSTANT,
        OutlineKind::Enum => SymbolKind::ENUM,
        OutlineKind::EnumVariant => SymbolKind::ENUM_MEMBER,
        OutlineKind::Extend | OutlineKind::Impl => SymbolKind::MODULE,
        OutlineKind::Function => SymbolKind::FUNCTION,
        OutlineKind::Method | OutlineKind::ProtocolMethod => SymbolKind::METHOD,
        OutlineKind::Protocol => SymbolKind::INTERFACE,
        OutlineKind::Test => SymbolKind::EVENT,
        OutlineKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
    }
}

fn document_symbols(nodes: &[OutlineNode], positions: &Positions<'_>) -> Vec<DocumentSymbol> {
    nodes
        .iter()
        .map(|node| {
            let children = document_symbols(&node.children, positions);
            #[allow(deprecated)]
            DocumentSymbol {
                name: node.name.clone(),
                detail: node.detail.clone(),
                kind: symbol_kind(node.kind),
                tags: None,
                deprecated: None,
                range: positions.range(&node.span),
                selection_range: positions.range(&node.name_span),
                children: (!children.is_empty()).then_some(children),
            }
        })
        .collect()
}

/// The flat entries of one file whose names contain `query`.
fn workspace_symbols(
    file: &File,
    query: &str,
    positions: &Positions<'_>,
    results: &mut Vec<SymbolInformation>,
) {
    let Some(uri) = file.path.as_deref().and_then(path_to_uri) else {
        return;
    };
    flatten(&outline(file), None, &mut |node, container| {
        if !query.is_empty() && !node.name.to_ascii_lowercase().contains(query) {
            return;
        }
        #[allow(deprecated)]
        results.push(SymbolInformation {
            name: node.name.clone(),
            kind: symbol_kind(node.kind),
            tags: None,
            deprecated: None,
            location: Location {
                uri: uri.clone(),
                range: positions.range(&node.span),
            },
            container_name: container.map(str::to_string),
        });
    });
}

/// Call `visit` on every node with the name of the declaration that
/// encloses it. An `impl` or `extend` block is a container, not a
/// symbol to jump to, so its own node is skipped.
fn flatten<'a>(
    nodes: &'a [OutlineNode],
    container: Option<&'a str>,
    visit: &mut impl FnMut(&'a OutlineNode, Option<&'a str>),
) {
    for node in nodes {
        if !matches!(node.kind, OutlineKind::Extend | OutlineKind::Impl) {
            visit(node, container);
        }
        flatten(&node.children, Some(&node.name), visit);
    }
}
