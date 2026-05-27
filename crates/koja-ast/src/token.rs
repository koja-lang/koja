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
    Break,
    Cond,
    Const,
    Else,
    End,
    Enum,
    Extend,
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
