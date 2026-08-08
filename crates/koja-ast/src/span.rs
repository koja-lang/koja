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

/// Index into the source file table of one compilation, in parse input
/// order. File ids are transient per compilation, like paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

impl FileId {
    /// Sentinel for bare-string parses (REPL, formatter, tests), which
    /// never cross file boundaries.
    pub const UNKNOWN: FileId = FileId(u32::MAX);
}

/// A source range defined by a start and end position, plus the file it
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: Position,
    pub end: Position,
    pub file: FileId,
    /// True on compiler-synthesized nodes (derived impls), which copy
    /// their declaring type's positions. Position lookups skip them.
    pub synthetic: bool,
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
    pub fn new(start: Position, end: Position, file: FileId) -> Self {
        Self {
            start,
            end,
            file,
            synthetic: false,
        }
    }

    /// A copy of this span marked as compiler-synthesized.
    pub fn as_synthetic(self) -> Span {
        Span {
            synthetic: true,
            ..self
        }
    }

    /// Merges two spans into one covering both. Keeps `self.file` and
    /// `self.synthetic`.
    pub fn to(self, end: Span) -> Span {
        Span {
            end: end.end,
            ..self
        }
    }

    /// Returns a zero-length span at the origin, used as a placeholder.
    pub fn zero() -> Self {
        let p = Position {
            offset: 0,
            line: 0,
            column: 0,
        };
        Self::new(p, p, FileId::UNKNOWN)
    }
}
