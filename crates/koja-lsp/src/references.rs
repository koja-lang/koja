//! Find references handler for the Koja LSP.
//!
//! Every occurrence of the symbol under the cursor across the
//! project files, from the reference index.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_query::Role;

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/references`.
    pub(crate) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        self.with_analysis(&uri, |doc| {
            let Some(symbol) = doc.symbol_at(position) else {
                return Ok(None);
            };
            let index = &doc.state.index;
            let mut locations: Vec<Location> = index
                .occurrences(symbol.key)
                .filter(|occurrence| include_declaration || occurrence.role != Role::Declaration)
                .map(|occurrence| Location {
                    uri: doc.uri_of(occurrence.span.file, &uri),
                    range: doc.range_of(&occurrence.span),
                })
                .collect();

            // A declaration outside the indexed files, such as a
            // stdlib type, still has a name span in the registry.
            if include_declaration
                && index.declaration(symbol.key).is_none()
                && let Some(span) = index.declaration_span(symbol.key, doc.analysis.registry)
            {
                locations.push(Location {
                    uri: doc.uri_of(span.file, &uri),
                    range: doc.range_of(&span),
                });
            }
            Ok(Some(locations))
        })
        .await
    }
}
