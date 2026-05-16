//! `impl Target [for Trait] ... end` and the inline `type` aliases
//! that may live inside an `impl` body.
//!
//! `impl Foo` is an inherent impl on `Foo`; `impl Trait for Foo` is
//! a trait impl. The parser reads the first type expression
//! unconditionally and only knows which form it has after looking
//! for the `for` keyword.

use expo_ast::ast::{ImplBlock, ImplMember, Item};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_impl_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // impl

        let first_type = self.parse_type_expr();

        let (target, trait_expr) = if self.eat(&TokenKind::For).is_some() {
            let actual_target = self.parse_type_expr();
            (actual_target, Some(first_type))
        } else {
            (first_type, None)
        };

        self.skip_newlines();
        let mut members = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    let func = self.parse_type_body_function("impl");
                    members.push(ImplMember::Function(func));
                }
                TokenKind::Type => {
                    let alias = self.parse_type_alias(Vec::new());
                    members.push(ImplMember::TypeAlias(alias));
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function or type alias in impl block, found {:?}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Impl(ImplBlock {
            target,
            trait_expr,
            members,
            span: self.span_from(start),
        })
    }
}
