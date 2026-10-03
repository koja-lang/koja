//! Shared conversion utilities for the Koja LSP.
//!
//! Koja spans count lines from 1 and columns from 1 in UTF-8 bytes.
//! LSP positions count lines from 0 and characters from 0 in the
//! encoding negotiated at `initialize`, UTF-16 unless the client
//! offers UTF-8. The two agree on ASCII and drift apart after any
//! multi-byte character on the line, so every conversion goes
//! through [`Positions`] over the text of the file in question.

use std::path::Path;

use tower_lsp_server::ls_types::*;

use koja_ast::span::Span;

/// How the client counts `Position.character`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    /// UTF-8 when the client lists it, otherwise the UTF-16 default
    /// every client supports.
    pub(crate) fn negotiate(capabilities: &ClientCapabilities) -> Self {
        let offered = capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_deref())
            .unwrap_or_default();
        if offered.contains(&PositionEncodingKind::UTF8) {
            Self::Utf8
        } else {
            Self::Utf16
        }
    }

    pub(crate) fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
        }
    }
}

/// Position conversion over one file's text.
#[derive(Clone, Copy)]
pub(crate) struct Positions<'a> {
    encoding: PositionEncoding,
    text: &'a str,
}

impl<'a> Positions<'a> {
    pub(crate) fn new(encoding: PositionEncoding, text: &'a str) -> Self {
        Self { encoding, text }
    }

    pub(crate) fn text(&self) -> &'a str {
        self.text
    }

    /// The LSP range of a Koja span.
    pub(crate) fn range(&self, span: &Span) -> Range {
        Range {
            start: self.position(&span.start),
            end: self.position(&span.end),
        }
    }

    /// The LSP position of a Koja position.
    pub(crate) fn position(&self, position: &koja_ast::span::Position) -> Position {
        let line = position.line.saturating_sub(1);
        let (_, text) = self.line(line);
        let byte_column = position.column.saturating_sub(1) as usize;
        Position::new(line, encoded_column(text, byte_column, self.encoding))
    }

    /// The Koja line and column under an LSP position, both counted
    /// from 1.
    pub(crate) fn line_column(&self, position: Position) -> (u32, u32) {
        let (_, text) = self.line(position.line);
        let byte_column = byte_column(text, position.character, self.encoding);
        (position.line + 1, byte_column as u32 + 1)
    }

    /// The byte offset of an LSP position, clamped to the text.
    pub(crate) fn offset(&self, position: Position) -> usize {
        let (start, text) = self.line(position.line);
        start + byte_column(text, position.character, self.encoding)
    }

    /// The start offset and text of a 0-indexed line, without its
    /// line break. A line past the end is empty at the text's end.
    fn line(&self, line: u32) -> (usize, &'a str) {
        let mut start = 0;
        for (index, raw) in self.text.split_inclusive('\n').enumerate() {
            if index == line as usize {
                return (start, raw.trim_end_matches(['\n', '\r']));
            }
            start += raw.len();
        }
        (self.text.len(), "")
    }
}

/// The encoded column of a byte column on `line`.
fn encoded_column(line: &str, byte_column: usize, encoding: PositionEncoding) -> u32 {
    let byte_column = clamp_to_boundary(line, byte_column);
    match encoding {
        PositionEncoding::Utf8 => byte_column as u32,
        PositionEncoding::Utf16 => line[..byte_column].encode_utf16().count() as u32,
    }
}

/// The byte column of an encoded column on `line`.
fn byte_column(line: &str, column: u32, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => clamp_to_boundary(line, column as usize),
        PositionEncoding::Utf16 => {
            let mut units = 0;
            for (offset, ch) in line.char_indices() {
                if units >= column {
                    return offset;
                }
                units += ch.len_utf16() as u32;
            }
            line.len()
        }
    }
}

/// `byte_column` moved back to the start of the character it falls
/// inside, and no further than the end of `line`.
fn clamp_to_boundary(line: &str, byte_column: usize) -> usize {
    let mut column = byte_column.min(line.len());
    while !line.is_char_boundary(column) {
        column -= 1;
    }
    column
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

#[cfg(test)]
mod tests {
    use koja_ast::span::FileId;

    use super::*;

    // `é` is two bytes and one UTF-16 unit. `😀` is four bytes and
    // two units.
    const TEXT: &str = "fn é\n  x = \"😀\" + y\n";

    fn koja(line: u32, column: u32) -> koja_ast::span::Position {
        koja_ast::span::Position {
            offset: 0,
            line,
            column,
        }
    }

    #[test]
    fn utf16_columns_shrink_after_multibyte_text() {
        let positions = Positions::new(PositionEncoding::Utf16, TEXT);
        // `y` starts at byte 15 on line 2 (1-indexed column 16).
        assert_eq!(positions.position(&koja(2, 16)), Position::new(1, 13));
        assert_eq!(positions.line_column(Position::new(1, 13)), (2, 16));
    }

    #[test]
    fn utf8_columns_are_bytes() {
        let positions = Positions::new(PositionEncoding::Utf8, TEXT);
        assert_eq!(positions.position(&koja(2, 16)), Position::new(1, 15));
        assert_eq!(positions.line_column(Position::new(1, 15)), (2, 16));
    }

    #[test]
    fn ascii_lines_agree_in_both_encodings() {
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            let positions = Positions::new(encoding, "fn run\n  1\nend\n");
            let span = Span::new(koja(3, 1), koja(3, 4), FileId(0));
            assert_eq!(
                positions.range(&span),
                Range::new(Position::new(2, 0), Position::new(2, 3))
            );
        }
    }

    #[test]
    fn offsets_count_bytes_from_the_text_start() {
        let positions = Positions::new(PositionEncoding::Utf16, TEXT);
        assert_eq!(positions.offset(Position::new(0, 0)), 0);
        // Line 2 starts after `fn é\n`, which is 6 bytes.
        assert_eq!(positions.offset(Position::new(1, 0)), 6);
        assert_eq!(positions.offset(Position::new(1, 13)), 6 + 15);
    }

    #[test]
    fn positions_past_the_end_clamp() {
        let positions = Positions::new(PositionEncoding::Utf16, "ab\n");
        assert_eq!(positions.offset(Position::new(0, 10)), 2);
        assert_eq!(positions.offset(Position::new(7, 0)), 3);
        assert_eq!(positions.line_column(Position::new(0, 10)), (1, 3));
    }

    #[test]
    fn negotiation_prefers_utf8_when_offered() {
        let mut capabilities = ClientCapabilities::default();
        assert_eq!(
            PositionEncoding::negotiate(&capabilities),
            PositionEncoding::Utf16
        );
        capabilities.general = Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF16,
                PositionEncodingKind::UTF8,
            ]),
            ..Default::default()
        });
        assert_eq!(
            PositionEncoding::negotiate(&capabilities),
            PositionEncoding::Utf8
        );
    }
}
