//! Document highlight handler for the Koja LSP.
//!
//! Every occurrence in the active file of the symbol under the
//! cursor, so the editor can mark them while the cursor rests on
//! one.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_query::Role;

use crate::backend::Backend;
use crate::convert::span_to_range;

impl Backend {
    /// Handles `textDocument/documentHighlight`.
    pub(crate) async fn handle_document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let Some(state) = docs.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(file) = analysis.file_id(&state.active_path) else {
            return Ok(None);
        };
        let Some(symbol) = state.symbol_at(&analysis, position) else {
            return Ok(None);
        };

        let highlights = state
            .index
            .occurrences(symbol.key)
            .filter(|occurrence| occurrence.span.file == file)
            .map(|occurrence| DocumentHighlight {
                range: span_to_range(&occurrence.span),
                kind: Some(match occurrence.role {
                    Role::Read => DocumentHighlightKind::READ,
                    Role::Declaration | Role::Write => DocumentHighlightKind::WRITE,
                }),
            })
            .collect();

        Ok(Some(highlights))
    }
}
