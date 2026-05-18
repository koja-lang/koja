//! `extend Type ... end` attaches inherent methods to `Type`.
//! Members share `impl`'s grammar; bodies are parsed via
//! [`Parser::parse_impl_members`].

use expo_ast::ast::{ExtendBlock, Item};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_extend_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance();

        let target = self.parse_type_expr();
        let members = self.parse_impl_members();
        self.expect(&TokenKind::End);

        Item::Extend(ExtendBlock {
            target,
            members,
            span: self.span_from(start),
        })
    }
}
