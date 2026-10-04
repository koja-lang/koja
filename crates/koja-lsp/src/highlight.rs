//! Document highlight handler for the Koja LSP.
//!
//! Every occurrence in the active file of the symbol under the
//! cursor, so the editor can mark them while the cursor rests on
//! one.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_query::Role;

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/documentHighlight`.
    pub(crate) async fn handle_document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        self.with_analysis(&uri, |doc| {
            let Some(symbol) = doc.symbol_at(position) else {
                return Ok(None);
            };
            let highlights = doc
                .state
                .index
                .occurrences(symbol.key)
                .filter(|occurrence| occurrence.span.file == doc.file)
                .map(|occurrence| DocumentHighlight {
                    range: doc.positions.range(&occurrence.span),
                    kind: Some(match occurrence.role {
                        Role::Read => DocumentHighlightKind::READ,
                        Role::Declaration | Role::Write => DocumentHighlightKind::WRITE,
                    }),
                })
                .collect();
            Ok(Some(highlights))
        })
        .await
    }
}
