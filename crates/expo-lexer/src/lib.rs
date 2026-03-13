mod lexer;

pub use expo_ast::ast::Comment;
pub use expo_ast::span::{Position, Span};
pub use expo_ast::token::{Token, TokenKind};
pub use lexer::{LexResult, lex};
