//! Rename handlers for the Koja LSP.
//!
//! `prepareRename` validates the symbol under the cursor and returns
//! its range and current name. `rename` validates again with the new
//! name and returns one `WorkspaceEdit` over every file that mentions
//! the symbol. Both report a refusal as a request error so the editor
//! shows the reason. The rules live in [`koja_query::rename`].

use std::collections::HashMap;

use tower_lsp_server::jsonrpc::{Error, Result};
use tower_lsp_server::ls_types::*;

use koja_query::rename::{Rename, RenameRefusal, prepare_rename, validate_new_name};

use crate::backend::Backend;
use crate::document::Doc;

impl Backend {
    /// Handles `textDocument/prepareRename`.
    pub(crate) async fn handle_prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let position = params.position;

        self.with_analysis(&uri, |doc| {
            let Some(symbol) = doc.symbol_at(position) else {
                return Ok(None);
            };
            let rename = plan(&doc, symbol.key).map_err(refusal_error)?;

            // Offer the range of the occurrence under the cursor, not
            // the declaration, so the editor's inline box opens in
            // place.
            let (line, column) = doc.line_column(position);
            let range = doc
                .state
                .index
                .occurrence_at(doc.file, line, column)
                .map(|occurrence| doc.range_of(&occurrence.span))
                .unwrap_or_else(|| doc.range_of(&rename.declaration));
            Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
                range,
                placeholder: rename.name,
            }))
        })
        .await
    }

    /// Handles `textDocument/rename`.
    pub(crate) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;

        self.with_analysis(&uri, |doc| {
            let Some(symbol) = doc.symbol_at(position) else {
                return Ok(None);
            };
            let rename = plan(&doc, symbol.key).map_err(refusal_error)?;
            validate_new_name(&rename.name, &new_name).map_err(refusal_error)?;

            let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
            for span in &rename.spans {
                let target = doc.uri_of(span.file, &uri);
                changes.entry(target).or_default().push(TextEdit {
                    range: doc.range_of(span),
                    new_text: new_name.clone(),
                });
            }
            Ok(Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }))
        })
        .await
    }
}

fn plan(doc: &Doc<'_>, key: koja_query::SymbolKey) -> std::result::Result<Rename, RenameRefusal> {
    prepare_rename(&doc.analysis, &doc.state.index, key, |file| {
        doc.analysis
            .path_of(file)
            .is_some_and(|path| doc.state.is_project_file(path))
    })
}

fn refusal_error(refusal: RenameRefusal) -> Error {
    Error::invalid_params(refusal.to_string())
}
