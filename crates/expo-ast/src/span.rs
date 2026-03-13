#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub offset: u32,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    pub fn zero() -> Self {
        let p = Position {
            offset: 0,
            line: 0,
            column: 0,
        };
        Self { start: p, end: p }
    }
}
