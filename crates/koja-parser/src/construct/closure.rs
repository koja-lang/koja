//! Closure expressions. Two surface forms:
//!
//! - Block closure: `fn(params) -> ReturnType ... end`. Full
//!   parameter list with optional types and an explicit body.
//! - Short closure: `expr -> expr`. The Pratt loop in [`crate::expr`]
//!   recognises the `->` and calls [`Parser::expr_to_closure_params`]
//!   to reinterpret the already-parsed LHS as a parameter shape.

use koja_ast::ast::{ClosureParam, Expr, ExprKind, PassMode};
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_closure_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // fn

        self.expect(&TokenKind::LParen);
        let params = self.comma_separated(&TokenKind::RParen, Self::parse_closure_param);
        self.expect(&TokenKind::RParen);

        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_expr())
        } else {
            None
        };

        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::Closure {
                params,
                return_type,
                body,
            },
            self.span_from(start),
        )
    }

    fn parse_closure_param(&mut self) -> ClosureParam {
        let start = self.current_span();
        let mode = if self.eat(&TokenKind::Move).is_some() {
            PassMode::Move
        } else {
            PassMode::Borrow
        };
        match self.peek().clone() {
            TokenKind::Ident(name) if name == "_" => {
                self.advance();
                ClosureParam::Wildcard {
                    local_id: None,
                    span: self.span_from(start),
                }
            }
            TokenKind::Ident(name) => {
                self.advance();
                let type_expr = if self.eat(&TokenKind::Colon).is_some() {
                    Some(self.parse_type_expr())
                } else {
                    None
                };
                ClosureParam::Name {
                    local_id: None,
                    mode,
                    name,
                    span: self.span_from(start),
                    type_expr,
                }
            }
            TokenKind::LParen => {
                self.advance(); // (
                let names = self.comma_separated(&TokenKind::RParen, Self::expect_ident);
                self.expect(&TokenKind::RParen);
                ClosureParam::Destructured {
                    names,
                    span: self.span_from(start),
                }
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected closure parameter, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                ClosureParam::Wildcard {
                    local_id: None,
                    span,
                }
            }
        }
    }

    /// Reinterpret the LHS of a short-closure arrow as its parameter
    /// list. `x -> ...` → `[x]`; `_ -> ...` → wildcard; `(x) -> ...`
    /// → unwrap the Group and recurse. Anything else is a syntax
    /// error.
    pub(crate) fn expr_to_closure_params(&mut self, expr: &Expr, span: Span) -> Vec<ClosureParam> {
        match &expr.kind {
            ExprKind::Ident { name, .. } if name == "_" => {
                vec![ClosureParam::Wildcard {
                    local_id: None,
                    span: expr.span,
                }]
            }
            ExprKind::Ident { name, .. } => {
                vec![ClosureParam::Name {
                    local_id: None,
                    mode: PassMode::Borrow,
                    name: name.clone(),
                    span: expr.span,
                    type_expr: None,
                }]
            }
            ExprKind::Group { expr: inner, .. } => self.expr_to_closure_params(inner, span),
            _ => {
                self.error("invalid closure parameter list".to_string(), span);
                vec![ClosureParam::Wildcard {
                    local_id: None,
                    span,
                }]
            }
        }
    }
}
