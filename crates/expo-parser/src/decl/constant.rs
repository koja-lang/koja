//! `const NAME [: T] = expr` at the module level.
//!
//! Constant names may be either `Ident` (lowercase) or `TypeIdent`
//! (PascalCase) — both shapes are accepted at the syntax layer.
//! Anything else lands a guiding diagnostic and proceeds with an
//! error sentinel so later phases can still walk the rest of the
//! file.

use expo_ast::ast::{Annotation, Constant, Item};
use expo_ast::token::TokenKind;

use crate::parser::{ERROR_IDENT, Parser};

impl Parser {
    pub(crate) fn parse_constant_item(&mut self, annotations: Vec<Annotation>) -> Item {
        let start = self.current_span();
        self.expect(&TokenKind::Const);
        let name = match self.peek().clone() {
            TokenKind::Ident(name) | TokenKind::TypeIdent(name) => {
                self.advance();
                name
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected constant name, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                ERROR_IDENT.to_string()
            }
        };
        let type_annotation = if self.peek() == &TokenKind::Colon {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        self.expect(&TokenKind::Eq);
        let value = self.parse_expr();
        Item::Constant(Constant {
            annotations,
            name,
            type_annotation,
            value,
            span: self.span_from(start),
        })
    }
}
