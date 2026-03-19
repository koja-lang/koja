//! Shared conversion utilities for the Expo LSP.
//!
//! Provides common helpers for span-to-range conversion, module file
//! resolution, and doc extraction from external files.

use std::path::PathBuf;

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

/// Resolves a module import path to a `.expo` file on disk, relative to
/// the currently open document.
pub(crate) fn resolve_module_file(current_uri: &Uri, module_path: &[String]) -> Option<PathBuf> {
    let uri_str = current_uri.as_str();
    let file_path = uri_str.strip_prefix("file://")?;
    let current = PathBuf::from(file_path);
    let dir = current.parent()?;

    let mut candidate = dir.to_path_buf();
    for segment in module_path {
        candidate.push(segment);
    }
    candidate.set_extension("expo");

    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Parses the file at `uri_str` and extracts the `@doc` annotation for
/// the item named `name`.
pub(crate) fn find_doc_from_uri(uri_str: &str, name: &str) -> Option<String> {
    let path = uri_str.strip_prefix("file://")?;
    let source = std::fs::read_to_string(path).ok()?;
    let parsed = expo_parser::parse(&source);
    crate::lookup::find_doc_for(&parsed.module, name)
}
