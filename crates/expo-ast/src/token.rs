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
    None_,

    // Keywords
    And,
    Arena,
    Await,
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
    Import,
    In,
    Loop,
    Match,
    Move,
    Not,
    Or,
    Priv,
    Protocol,
    Receive,
    Ref,
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
    LtEq,
    GtEq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Arrow,
    ColonColon,
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

    // Structural
    Newline,
    Eof,
}

impl TokenKind {
    /// Returns `true` if this token is a reserved keyword.
    pub fn is_keyword(&self) -> bool {
        matches!(
            self,
            TokenKind::And
                | TokenKind::Arena
                | TokenKind::Await
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
                | TokenKind::Import
                | TokenKind::In
                | TokenKind::Loop
                | TokenKind::Match
                | TokenKind::Move
                | TokenKind::None_
                | TokenKind::Not
                | TokenKind::Or
                | TokenKind::Priv
                | TokenKind::Protocol
                | TokenKind::Receive
                | TokenKind::Ref
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
