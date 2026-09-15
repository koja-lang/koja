//! Cursor position tests against spans.

use koja_ast::span::{Position, Span};

/// True when the 1-indexed `(line, col)` cursor falls inside `span`.
/// A cursor directly after the last character counts as inside, so a
/// cursor at the end of a word still finds it. Synthetic spans never
/// match. They copy a user node's positions and would shadow it.
pub fn span_contains(span: &Span, line: u32, col: u32) -> bool {
    if span.synthetic {
        return false;
    }
    if line < span.start.line || line > span.end.line {
        return false;
    }
    if line == span.start.line && col < span.start.column {
        return false;
    }
    if line == span.end.line && col > span.end.column {
        return false;
    }
    true
}

/// The span of the last `.`-separated segment of a path that ends at
/// `span.end`. Static receivers are rewritten to one `Ident` whose
/// name is the joined path, so the name of the type it resolves to
/// is the tail of that text.
pub fn tail_segment_span(span: Span, text: &str) -> Span {
    let tail = text.rsplit('.').next().unwrap_or(text);
    let len = tail.chars().count() as u32;
    Span {
        start: Position {
            offset: span.end.offset.saturating_sub(len),
            line: span.end.line,
            column: span.end.column.saturating_sub(len),
        },
        ..span
    }
}
