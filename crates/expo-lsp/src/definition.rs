//! Go-to-definition handler for the Expo LSP.
//!
//! Resolves the definition location for functions, structs, enums,
//! constants, and module references.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::Item;

use crate::backend::Backend;
use crate::convert::span_to_range;
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
            SymbolInfo::Variable { .. } => None,
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
