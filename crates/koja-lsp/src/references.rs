//! Find references handler for the Koja LSP.
//!
//! Every occurrence of the symbol under the cursor across the
//! project files, from the reference index.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_query::Role;

use crate::backend::Backend;
use crate::convert::span_to_range;

impl Backend {
    /// Handles `textDocument/references`.
    pub(crate) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

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

        let mut locations: Vec<Location> = state
            .index
            .occurrences(symbol.key)
            .filter(|occurrence| include_declaration || occurrence.role != Role::Declaration)
            .map(|occurrence| Location {
                uri: state.uri_of(&analysis, occurrence.span.file, &uri),
                range: span_to_range(&occurrence.span),
            })
            .collect();

        // A declaration outside the indexed files, such as a stdlib
        // type, still has a name span in the registry.
        if include_declaration
            && state.index.declaration(symbol.key).is_none()
            && let Some(span) = state.index.declaration_span(symbol.key, analysis.registry)
        {
            locations.push(Location {
                uri: state.uri_of(&analysis, span.file, &uri),
                range: span_to_range(&span),
            });
        }

        Ok(Some(locations))
    }
}
