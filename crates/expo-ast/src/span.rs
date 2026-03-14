/// A byte offset with line and column numbers within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub offset: u32,
    pub line: u32,
    pub column: u32,
}

/// A source range defined by a start and end position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
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
