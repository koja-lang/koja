//! `if` expressions.
//!
//! `if` may carry an `else` branch but does not accept `else if`
//! (the error path nudges users toward `cond` for multi-way
//! branching). `unless` was removed in 0.19 but stays reserved so
//! the parser can point at the `if not` replacement.

use koja_ast::ast::{Expr, ExprKind, UnaryOp};
use koja_ast::token::TokenKind;

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
                    "`else if` is not valid".to_string(),
                    "use `cond` for multi-way branching".to_string(),
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

    /// Reports the removed `unless` form and recovers by parsing it
    /// as `if not cond ... end`, so later passes see well-formed code
    /// and report nothing spurious.
    pub(crate) fn parse_unless_removed(&mut self) -> Expr {
        let start = self.current_span();
        self.error_with_hint(
            "`unless` was removed in 0.19".to_string(),
            "write `if not cond` instead".to_string(),
            start,
        );
        self.advance(); // unless

        let condition = self.parse_expr();
        let condition_span = condition.span;
        let then_body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::If {
                condition: Box::new(Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        operand: Box::new(condition),
                    },
                    condition_span,
                )),
                then_body,
                else_body: None,
            },
            self.span_from(start),
        )
    }
}
