//! Shared conversion utilities for the Expo LSP.

use tower_lsp_server::ls_types::*;

use expo_ast::span::Span;

/// Converts an Expo compiler [`Span`] to an LSP [`Range`].
///
/// Expo spans are 1-indexed; LSP ranges are 0-indexed.
pub(crate) fn span_to_range(span: &Span) -> Range {
    Range {
        start: Position::new(
            span.start.line.saturating_sub(1),
            span.start.column.saturating_sub(1),
        ),
        end: Position::new(
            span.end.line.saturating_sub(1),
            span.end.column.saturating_sub(1),
        ),
    }
}
