//! Inlay hint handler for the Koja LSP.
//!
//! Inferred binding types after a name and parameter names before
//! positional arguments, for the range the editor has on screen.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::span::{FileId, Span};
use koja_query::inlay::{self, HintKind};

use crate::backend::Backend;
use crate::convert::Positions;

impl Backend {
    /// Handles `textDocument/inlayHint`.
    pub(crate) async fn handle_inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;

        self.with_analysis(&uri, |doc| {
            let range = range_to_span(&params.range, doc.file, &doc.positions);
            let hints = inlay::hints(&doc.analysis, &doc.state.index, doc.file, range)
                .into_iter()
                .map(|hint| InlayHint {
                    position: doc.positions.position(&hint.position),
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
        })
        .await
    }
}

/// The inverse of `Positions::range`. Offsets stay zero because the
/// range only filters by line and column.
fn range_to_span(range: &Range, file: FileId, positions: &Positions<'_>) -> Span {
    let position = |p: Position| {
        let (line, column) = positions.line_column(p);
        koja_ast::span::Position {
            offset: 0,
            line,
            column,
        }
    };
    Span {
        start: position(range.start),
        end: position(range.end),
        file,
        synthetic: false,
    }
}
