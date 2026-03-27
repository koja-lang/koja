//! Lexer for Expo source code.
//!
//! Converts a source string into a flat token stream, extracting comments
//! into a separate list and reporting lexical errors as diagnostics.

mod cursor;
mod lexer;

pub use expo_ast::ast::Comment;
pub use expo_ast::span::{Position, Span};
pub use expo_ast::token::{Token, TokenKind};
pub use lexer::{LexResult, lex};
