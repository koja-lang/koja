//! Go-to-definition handler for the Koja LSP.
//!
//! Resolves the symbol under the cursor through the reference index
//! and lands on the name of its declaration, in this project or in
//! the stdlib.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/definition` requests by resolving the symbol
    /// under the cursor to its definition location.
    pub(crate) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        self.with_analysis(&uri, |doc| {
            let Some(symbol) = doc.symbol_at(position) else {
                return Ok(None);
            };
            let Some(span) = doc
                .state
                .index
                .declaration_span(symbol.key, doc.analysis.registry)
            else {
                return Ok(None);
            };
            Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: doc.uri_of(span.file, &uri),
                range: doc.range_of(&span),
            })))
        })
        .await
    }
}
