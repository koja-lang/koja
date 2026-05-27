//! Span utilities for cursor-position-based symbol lookup.

use koja_ast::ast::{Annotation, AnnotationValue};
use koja_ast::span::Span;

/// Returns `true` if the 1-indexed `(line, col)` cursor position falls
/// within the given span.
pub(crate) fn span_contains(span: &Span, line: u32, col: u32) -> bool {
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

/// Returns `true` if the cursor is on the name portion of the span's
/// start line.
pub(crate) fn span_contains_name(_name: &str, span: &Span, line: u32, col: u32) -> bool {
    span.start.line == line && col >= span.start.column && col <= span.end.column
}

/// Extracts the doc string from a `@doc` annotation, if present.
pub(crate) fn annotation_doc(annotations: &[Annotation]) -> Option<String> {
    annotations
        .iter()
        .find(|a| a.name == "doc")
        .and_then(|a| match &a.value {
            Some(AnnotationValue::String(s)) => Some(s.clone()),
            _ => None,
        })
}
