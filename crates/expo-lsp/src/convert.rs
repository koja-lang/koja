//! Shared conversion utilities for the Expo LSP.

use std::path::Path;

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

/// Extracts a file system path from a `file://` URI.
pub(crate) fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(std::path::PathBuf::from(percent_decode(rest)))
}

/// Converts a file system path to a `file://` URI.
pub(crate) fn path_to_uri(path: &Path) -> Option<Uri> {
    Uri::from_file_path(path)
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next().and_then(|c| (c as char).to_digit(16));
            let lo = bytes.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8 as char);
            }
        } else {
            out.push(b as char);
        }
    }
    out
}
