use crate::span::Span;

/// A lexed token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// The kind of a lexed token.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Identifiers
    Ident(String),
    TypeIdent(String),

    // Literals
    IntLit(String),
    FloatLit(String),
    StringStart,
    StringFragment(String),
    StringEnd,
    InterpolStart,
    InterpolEnd,
    MultilineStringStart,
    MultilineStringEnd,
    True,
    False,

    // Keywords
    After,
    Arena,
    Break,
    Cond,
    Const,
    Else,
    End,
    Enum,
    Fn,
    For,
    If,
    Impl,
    In,
    Loop,
    Match,
    Move,
    Not,
    Priv,
    Protocol,
    Receive,
    Return,
    Self_,
    Shared,
    Spawn,
    Struct,
    Type,
    Unless,
    When,
    While,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtLt,
    GtGt,
    LtGt,
    LtEq,
    GtEq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Arrow,
    Pipe,
    Alias,
    Ampersand,
    Question,
    At,
    Dot,

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    ColonColon,

    // Structural
    Newline,
    EndOfFile,
}

impl TokenKind {
    /// Returns `true` if this token is a reserved keyword.
    pub fn is_keyword(&self) -> bool {
        matches!(
            self,
            TokenKind::After
                | TokenKind::Alias
                | TokenKind::Arena
                | TokenKind::Break
                | TokenKind::Cond
                | TokenKind::Const
                | TokenKind::Else
                | TokenKind::End
                | TokenKind::Enum
                | TokenKind::False
                | TokenKind::Fn
                | TokenKind::For
                | TokenKind::If
                | TokenKind::Impl
                | TokenKind::In
                | TokenKind::Loop
                | TokenKind::Match
                | TokenKind::Move
                | TokenKind::Not
                | TokenKind::Priv
                | TokenKind::Protocol
                | TokenKind::Receive
                | TokenKind::Return
                | TokenKind::Self_
                | TokenKind::Shared
                | TokenKind::Spawn
                | TokenKind::Struct
                | TokenKind::True
                | TokenKind::Type
                | TokenKind::Unless
                | TokenKind::When
                | TokenKind::While
        )
    }
}
