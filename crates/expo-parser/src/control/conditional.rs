//! `if` and `unless` expressions.
//!
//! `if` may carry an `else` branch but does not accept `else if`
//! (the error path nudges users toward `cond` for multi-way
//! branching). `unless` is the negated single-branch form and has
//! no `else`.

use expo_ast::ast::{Expr, ExprKind};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_if_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // if

        let condition = self.parse_expr();
        let then_body = self.parse_block();

        let else_body = if self.eat(&TokenKind::Else).is_some() {
            if *self.peek() == TokenKind::If {
                self.error_with_hint(
                    "else if is not supported".to_string(),
                    "use cond for multi-way branching".to_string(),
                    self.current_span(),
                );
            }
            Some(self.parse_block())
        } else {
            None
        };
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::If {
                condition: Box::new(condition),
                then_body,
                else_body,
            },
            self.span_from(start),
        )
    }

    pub(crate) fn parse_unless_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // unless

        let condition = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::Unless {
                condition: Box::new(condition),
                body,
            },
            self.span_from(start),
        )
    }
}
