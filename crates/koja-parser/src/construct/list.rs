//! Bracket and paren grouping expressions: list literals `[1, 2, 3]`,
//! map literals `[key: val, ...]` (with `[:]` as the empty map),
//! parenthesized expressions `(e)`, and the unit literal `()`.
//!
//! Tuples are rejected with a "use a struct instead" diagnostic so
//! the surface stays small without leaving the syntactic hole
//! confused for grouping.

use koja_ast::ast::{Expr, ExprKind, Literal};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_paren_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // (

        self.skip_newlines();
        if self.eat(&TokenKind::RParen).is_some() {
            return Expr::new(
                ExprKind::Literal {
                    value: Literal::Unit,
                },
                self.span_from(start),
            );
        }

        let first = self.parse_expr();

        if self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            while !self.at(&TokenKind::RParen) && !self.at_eof() {
                self.parse_expr();
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
                self.skip_newlines();
            }
            self.skip_newlines();
            self.expect(&TokenKind::RParen);
            let span = self.span_from(start);
            self.error(
                "tuples are not supported, use a struct instead".to_string(),
                span,
            );
            Expr::new(
                ExprKind::Literal {
                    value: Literal::Unit,
                },
                span,
            )
        } else {
            self.skip_newlines();
            self.expect(&TokenKind::RParen);
            Expr::new(
                ExprKind::Group {
                    expr: Box::new(first),
                },
                self.span_from(start),
            )
        }
    }

    pub(crate) fn parse_list_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // [

        self.skip_newlines();

        // Empty map literal: [:]
        if self.at(&TokenKind::Colon) && self.peek_nth(1) == &TokenKind::RBracket {
            self.advance(); // :
            self.advance(); // ]
            return Expr::new(
                ExprKind::Map {
                    entries: Vec::new(),
                },
                self.span_from(start),
            );
        }

        if self.at(&TokenKind::RBracket) {
            self.advance(); // ]
            return Expr::new(
                ExprKind::List {
                    elements: Vec::new(),
                },
                self.span_from(start),
            );
        }

        let first = self.parse_expr();

        // If followed by `:`, this is a map literal
        if self.eat(&TokenKind::Colon).is_some() {
            self.skip_newlines();
            let first_val = self.parse_expr();
            let mut entries = vec![(first, first_val)];
            while self.eat(&TokenKind::Comma).is_some() {
                self.skip_newlines();
                if self.at(&TokenKind::RBracket) {
                    break;
                }
                let key = self.parse_expr();
                self.expect(&TokenKind::Colon);
                self.skip_newlines();
                let val = self.parse_expr();
                entries.push((key, val));
            }
            self.skip_newlines();
            self.expect(&TokenKind::RBracket);
            return Expr::new(ExprKind::Map { entries }, self.span_from(start));
        }

        // Otherwise it's a list literal. The first element is
        // already parsed; route the tail through `comma_separated`
        // by manually consuming the comma that should follow it.
        let mut elements = vec![first];
        if self.eat(&TokenKind::Comma).is_some() {
            elements.extend(self.comma_separated(&TokenKind::RBracket, Self::parse_expr));
        }
        self.skip_newlines();
        self.expect(&TokenKind::RBracket);

        Expr::new(ExprKind::List { elements }, self.span_from(start))
    }
}
