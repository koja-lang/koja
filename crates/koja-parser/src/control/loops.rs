//! Looping expressions: `for`, `loop`, `while`.
//!
//! `for pattern in iterable ... end` desugars to a loop in later
//! phases. `loop` is unbounded, and `while condition ... end` is the
//! conditioned form. All three share the same single-block body
//! shape terminated by `end`.

use koja_ast::ast::{Expr, ExprKind};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_for_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // for

        let pattern = self.parse_pattern();
        self.expect(&TokenKind::In);
        let iterable = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::For {
                pattern,
                iterable: Box::new(iterable),
                body,
            },
            self.span_from(start),
        )
    }

    pub(crate) fn parse_loop_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // loop
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(ExprKind::Loop { body }, self.span_from(start))
    }

    pub(crate) fn parse_while_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // while
        let condition = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::While {
                condition: Box::new(condition),
                body,
            },
            self.span_from(start),
        )
    }
}
