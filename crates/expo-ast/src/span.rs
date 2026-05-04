//! Source location tracking for diagnostics, IDE features, and AST annotation.
//!
//! Every AST node carries a [`Span`] that records where it appeared in the
//! source file. Spans are defined by a start and end [`Position`], each
//! storing byte offset, line, and column.

use std::fmt;

/// A byte offset with line and column numbers within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    pub offset: u32,
    pub line: u32,
    pub column: u32,
}

/// A source range defined by a start and end position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

/// Compact `L:C-L:C` rendering shared by the AST printer and the
/// registry printer. Callers prepend `@` if they want the `@L:C-L:C`
/// convention the AST tree uses.
impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}-{}:{}",
            self.start.line, self.start.column, self.end.line, self.end.column,
        )
    }
}

impl Default for Span {
    fn default() -> Self {
        Self::zero()
    }
}

impl Span {
    /// Creates a span from explicit start and end positions.
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// Returns a zero-length span at the origin, used as a placeholder.
    pub fn zero() -> Self {
        let p = Position {
            offset: 0,
            line: 0,
            column: 0,
        };
        Self { start: p, end: p }
    }
}
