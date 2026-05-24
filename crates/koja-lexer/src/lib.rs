//! Lexer for Koja source code.
//!
//! Converts a source string into a flat token stream, extracting comments
//! into a separate list and reporting lexical errors as diagnostics.

mod cursor;
mod lexer;

pub use koja_ast::ast::Comment;
pub use koja_ast::span::{Position, Span};
pub use koja_ast::token::{Token, TokenKind};
pub use lexer::{LexResult, lex};
