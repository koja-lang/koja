//! Go-to-definition handler for the Expo LSP.
//!
//! Resolves the definition location for functions, structs, enums,
//! constants, and module references.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::Item;

use crate::backend::Backend;
use crate::convert::{resolve_module_file, span_to_range};
use crate::lookup::{self, SymbolInfo};

impl Backend {
    /// Handles `textDocument/definition` requests by resolving the symbol
    /// under the cursor to its definition location.
    pub(crate) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let line = pos.line + 1;
        let col = pos.character + 1;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let symbol = match lookup::find_symbol_at(&state.module, line, col, &state.ctx) {
            Some(s) => s,
            None => return Ok(None),
        };

        if let SymbolInfo::Module { ref path } = symbol {
            let module_name = path.join(".");
            let target_uri_str = state.module_uris.get(&module_name).cloned().or_else(|| {
                resolve_module_file(&uri, path).map(|p| format!("file://{}", p.display()))
            });
            if let Some(uri_str) = target_uri_str {
                let target_uri: Uri = uri_str.parse().map_err(|_| {
                    tower_lsp_server::jsonrpc::Error::invalid_params("invalid module URI")
                })?;
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_uri,
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(0, 0),
                    },
                })));
            }
            return Ok(None);
        }

        if let SymbolInfo::ModuleFunction {
            ref module,
            ref name,
        } = symbol
        {
            if let Some(module_uri) = state.module_uris.get(module) {
                return goto_definition_in_file(module_uri, name);
            }
            return Ok(None);
        }

        let symbol_name = match &symbol {
            SymbolInfo::Function { name }
            | SymbolInfo::Struct { name }
            | SymbolInfo::Enum { name }
            | SymbolInfo::Constant { name } => Some(name.as_str()),
            _ => None,
        };

        if let Some(name) = symbol_name
            && let Some(origin_uri_str) = state.imported_origins.get(name)
        {
            return goto_definition_in_file(origin_uri_str, name);
        }

        let def_span = match &symbol {
            SymbolInfo::Function { name } => state.ctx.functions.get(name).map(|sig| sig.span),
            SymbolInfo::Struct { name } | SymbolInfo::Enum { name } => {
                state.ctx.types.get(name).map(|info| info.span)
            }
            SymbolInfo::Constant { name } => state.module.items.iter().find_map(|item| {
                if let Item::Constant(c) = item
                    && c.name == *name
                {
                    Some(c.span)
                } else {
                    None
                }
            }),
            SymbolInfo::Variable { .. }
            | SymbolInfo::Module { .. }
            | SymbolInfo::ModuleFunction { .. } => None,
        };

        let span = match def_span {
            Some(s) => s,
            None => return Ok(None),
        };

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range: span_to_range(&span),
        })))
    }
}

/// Resolves a definition in an external file by parsing it and looking up
/// the symbol by name.
fn goto_definition_in_file(uri_str: &str, name: &str) -> Result<Option<GotoDefinitionResponse>> {
    let path = match uri_str.strip_prefix("file://") {
        Some(p) => p,
        None => return Ok(None),
    };
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let parsed = expo_parser::parse(&source);
    let ctx = expo_typecheck::collect_module(&parsed.module);

    let span = ctx
        .functions
        .get(name)
        .map(|sig| sig.span)
        .or_else(|| ctx.types.get(name).map(|info| info.span));

    let span = match span {
        Some(s) => s,
        None => return Ok(None),
    };

    let target_uri: Uri = uri_str
        .parse()
        .map_err(|_| tower_lsp_server::jsonrpc::Error::invalid_params("invalid URI"))?;

    Ok(Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: span_to_range(&span),
    })))
}
