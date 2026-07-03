use std::fmt;

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

impl fmt::Display for TokenKind {
    /// User-facing rendering for diagnostics: named tokens include their
    /// text (``identifier `foo` ``), keywords carry a `keyword` prefix,
    /// fixed lexemes are backticked, and structural tokens read as words.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ident(name) => write!(f, "identifier `{name}`"),
            Self::TypeIdent(name) => write!(f, "type identifier `{name}`"),
            Self::IntLit(text) | Self::FloatLit(text) => write!(f, "number `{text}`"),
            Self::StringFragment(_) => f.write_str("string text"),
            Self::Newline => f.write_str("newline"),
            Self::EndOfFile => f.write_str("end of file"),
            other => match other.keyword_lexeme() {
                Some(keyword) => write!(f, "keyword `{keyword}`"),
                None => write!(f, "`{}`", other.symbol_lexeme()),
            },
        }
    }
}

impl TokenKind {
    /// Source text for word-shaped tokens (keywords plus `true`,
    /// `false`, `self`, and `alias`).
    fn keyword_lexeme(&self) -> Option<&'static str> {
        Some(match self {
            Self::After => "after",
            Self::Alias => "alias",
            Self::Break => "break",
            Self::Cond => "cond",
            Self::Const => "const",
            Self::Else => "else",
            Self::End => "end",
            Self::Enum => "enum",
            Self::Extend => "extend",
            Self::False => "false",
            Self::Fn => "fn",
            Self::For => "for",
            Self::If => "if",
            Self::Impl => "impl",
            Self::In => "in",
            Self::Loop => "loop",
            Self::Match => "match",
            Self::Not => "not",
            Self::Priv => "priv",
            Self::Protocol => "protocol",
            Self::Receive => "receive",
            Self::Return => "return",
            Self::Self_ => "self",
            Self::Spawn => "spawn",
            Self::Struct => "struct",
            Self::True => "true",
            Self::Type => "type",
            Self::Unless => "unless",
            Self::When => "when",
            Self::While => "while",
            _ => return None,
        })
    }

    /// Source text for operator, delimiter, and string-delimiter tokens.
    /// Only called from `Display` after every other shape is handled.
    fn symbol_lexeme(&self) -> &'static str {
        match self {
            Self::Ampersand => "&",
            Self::Arrow => "->",
            Self::At => "@",
            Self::Colon => ":",
            Self::ColonColon => "::",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::Eq => "=",
            Self::EqEq => "==",
            Self::Gt => ">",
            Self::GtEq => ">=",
            Self::GtGt => ">>",
            Self::InterpolEnd => "}",
            Self::InterpolStart => "#{",
            Self::LBrace => "{",
            Self::LBracket => "[",
            Self::LParen => "(",
            Self::Lt => "<",
            Self::LtEq => "<=",
            Self::LtGt => "<>",
            Self::LtLt => "<<",
            Self::Minus => "-",
            Self::MinusEq => "-=",
            Self::MultilineStringEnd | Self::MultilineStringStart => "\"\"\"",
            Self::NotEq => "!=",
            Self::Percent => "%",
            Self::Pipe => "|",
            Self::Plus => "+",
            Self::PlusEq => "+=",
            Self::Question => "?",
            Self::RBrace => "}",
            Self::RBracket => "]",
            Self::RParen => ")",
            Self::Slash => "/",
            Self::SlashEq => "/=",
            Self::Star => "*",
            Self::StarEq => "*=",
            Self::StringEnd | Self::StringStart => "\"",
            other => unreachable!("token {other:?} has no fixed lexeme"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_each_token_category() {
        let cases = [
            (TokenKind::Ident("foo".into()), "identifier `foo`"),
            (
                TokenKind::TypeIdent("Point".into()),
                "type identifier `Point`",
            ),
            (TokenKind::IntLit("42".into()), "number `42`"),
            (TokenKind::FloatLit("1.5".into()), "number `1.5`"),
            (TokenKind::Fn, "keyword `fn`"),
            (TokenKind::Self_, "keyword `self`"),
            (TokenKind::True, "keyword `true`"),
            (TokenKind::Alias, "keyword `alias`"),
            (TokenKind::Arrow, "`->`"),
            (TokenKind::LtGt, "`<>`"),
            (TokenKind::RParen, "`)`"),
            (TokenKind::ColonColon, "`::`"),
            (TokenKind::InterpolStart, "`#{`"),
            (TokenKind::MultilineStringStart, "`\"\"\"`"),
            (TokenKind::StringFragment("hi".into()), "string text"),
            (TokenKind::Newline, "newline"),
            (TokenKind::EndOfFile, "end of file"),
        ];
        for (token, expected) in cases {
            assert_eq!(token.to_string(), expected);
        }
    }
}
