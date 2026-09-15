//! Go-to-definition handler for the Koja LSP.
//!
//! Resolves the symbol under the cursor through the reference index
//! and lands on the name of its declaration, in this project or in
//! the stdlib.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use crate::backend::Backend;
use crate::convert::span_to_range;

impl Backend {
    /// Handles `textDocument/definition` requests by resolving the symbol
    /// under the cursor to its definition location.
    pub(crate) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let Some(state) = docs.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(symbol) = state.symbol_at(&analysis, position) else {
            return Ok(None);
        };
        let Some(span) = state.index.declaration_span(symbol.key, analysis.registry) else {
            return Ok(None);
        };

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: state.uri_of(&analysis, span.file, &uri),
            range: span_to_range(&span),
        })))
    }
}
