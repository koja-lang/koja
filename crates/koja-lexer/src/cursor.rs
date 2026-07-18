//! Generic character stream cursor for lexing.
//!
//! Tracks a UTF-8 byte offset alongside character-based lookahead, line,
//! and column positions.

use crate::Position;

/// A movable pointer into a character buffer with line/column tracking.
pub(crate) struct Cursor<'source> {
    byte_offset: usize,
    chars: Vec<char>,
    column: u32,
    line: u32,
    pos: usize,
    source: &'source str,
}

impl<'source> Cursor<'source> {
    /// Consume the current character, advance the position, and update
    /// line/column tracking.
    pub(crate) fn advance(&mut self) -> char {
        let c = self.chars[self.pos];
        self.byte_offset += c.len_utf8();
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        c
    }

    /// Returns true if the cursor is past the end of input.
    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// Returns the character at an absolute index, or `None` if out of bounds.
    pub(crate) fn char_at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    /// Total number of characters in the source.
    pub(crate) fn len(&self) -> usize {
        self.chars.len()
    }

    /// Creates a new cursor over the given source string, starting at
    /// line 1, column 1.
    pub(crate) fn new(source: &'source str) -> Self {
        Self {
            byte_offset: 0,
            chars: source.chars().collect(),
            column: 1,
            line: 1,
            pos: 0,
            source,
        }
    }

    /// The current UTF-8 byte offset into the source.
    pub(crate) fn offset(&self) -> usize {
        self.byte_offset
    }

    /// The current character index used by character-based lookahead.
    pub(crate) fn char_index(&self) -> usize {
        self.pos
    }

    /// The current character without advancing. Panics if `at_end()` is true.
    pub(crate) fn peek(&self) -> char {
        self.chars[self.pos]
    }

    /// The character at `offset` positions ahead of the current position,
    /// or `None` if out of bounds.
    pub(crate) fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// Current source position as a `Position` (offset, line, column).
    pub(crate) fn position(&self) -> Position {
        Position {
            offset: self.byte_offset as u32,
            line: self.line,
            column: self.column,
        }
    }

    /// Advance past characters while `pred` returns true.
    pub(crate) fn skip_while(&mut self, pred: fn(char) -> bool) {
        while !self.at_end() && pred(self.peek()) {
            self.advance();
        }
    }

    /// Copies source text from byte offset `start` to the current position.
    pub(crate) fn text_from(&self, start: usize) -> String {
        self.source[start..self.byte_offset].to_string()
    }
}
