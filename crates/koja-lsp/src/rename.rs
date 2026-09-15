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

use koja_query::Analysis;
use koja_query::rename::{Rename, RenameRefusal, prepare_rename, validate_new_name};

use crate::backend::{Backend, DocumentState};
use crate::convert::span_to_range;

impl Backend {
    /// Handles `textDocument/prepareRename`.
    pub(crate) async fn handle_prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let docs = self.documents.read().await;
        let Some(state) = docs.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(symbol) = state.symbol_at(&analysis, params.position) else {
            return Ok(None);
        };
        let rename = plan(state, &analysis, symbol.key).map_err(refusal_error)?;

        // Offer the range of the occurrence under the cursor, not the
        // declaration, so the editor's inline box opens in place.
        let Some(file) = analysis.file_id(&state.active_path) else {
            return Ok(None);
        };
        let range = state
            .index
            .occurrence_at(
                file,
                params.position.line + 1,
                params.position.character + 1,
            )
            .map(|occurrence| span_to_range(&occurrence.span))
            .unwrap_or_else(|| span_to_range(&rename.declaration));
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range,
            placeholder: rename.name,
        }))
    }

    /// Handles `textDocument/rename`.
    pub(crate) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let docs = self.documents.read().await;
        let Some(state) = docs.get(uri.as_str()) else {
            return Ok(None);
        };
        let Some(analysis) = state.analysis() else {
            return Ok(None);
        };
        let Some(symbol) = state.symbol_at(&analysis, params.text_document_position.position)
        else {
            return Ok(None);
        };
        let rename = plan(state, &analysis, symbol.key).map_err(refusal_error)?;
        validate_new_name(&rename.name, &params.new_name).map_err(refusal_error)?;

        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        for span in &rename.spans {
            let target = state.uri_of(&analysis, span.file, &uri);
            changes.entry(target).or_default().push(TextEdit {
                range: span_to_range(span),
                new_text: params.new_name.clone(),
            });
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }
}

fn plan(
    state: &DocumentState,
    analysis: &Analysis<'_>,
    key: koja_query::SymbolKey,
) -> std::result::Result<Rename, RenameRefusal> {
    prepare_rename(analysis, &state.index, key, |file| {
        analysis
            .path_of(file)
            .is_some_and(|path| state.is_project_file(path))
    })
}

fn refusal_error(refusal: RenameRefusal) -> Error {
    Error::invalid_params(refusal.to_string())
}
