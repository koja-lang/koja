//! Inlay hint handler for the Koja LSP.
//!
//! Inferred binding types after a name and parameter names before
//! positional arguments, for the range the editor has on screen.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::span::{FileId, Span};
use koja_query::inlay::{self, HintKind};

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/inlayHint`.
    pub(crate) async fn handle_inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;

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

        let range = range_to_span(&params.range, file);
        let hints = inlay::hints(&analysis, &state.index, file, range)
            .into_iter()
            .map(|hint| InlayHint {
                position: Position::new(
                    hint.position.line.saturating_sub(1),
                    hint.position.column.saturating_sub(1),
                ),
                label: InlayHintLabel::String(hint.label),
                kind: Some(match hint.kind {
                    HintKind::Type => InlayHintKind::TYPE,
                    HintKind::Parameter => InlayHintKind::PARAMETER,
                }),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(hint.kind == HintKind::Parameter),
                data: None,
            })
            .collect();

        Ok(Some(hints))
    }
}

/// The inverse of `span_to_range`. Offsets stay zero because the
/// range only filters by line and column.
fn range_to_span(range: &Range, file: FileId) -> Span {
    let position = |p: &Position| koja_ast::span::Position {
        offset: 0,
        line: p.line + 1,
        column: p.character + 1,
    };
    Span {
        start: position(&range.start),
        end: position(&range.end),
        file,
        synthetic: false,
    }
}
