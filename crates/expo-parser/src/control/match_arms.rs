//! Arm-stream expressions: `match`, `cond`, and `receive`.
//!
//! All three iterate a body of `pattern when guard -> body` arms
//! (with `match` also accepting or-patterns and `receive` accepting
//! an optional `after` timeout block). The shared shape is:
//!
//! - one or more arms, each terminated by a newline that separates
//!   it from the next;
//! - bodies that may run across multiple statements until the
//!   "looks like a new arm" heuristic ([`Parser::looks_like_new_arm`])
//!   fires;
//! - a stuck-progress error recovery loop so a malformed arm doesn't
//!   wedge the whole block.
//!
//! `cond` arms swap the leading `pattern` for an arbitrary expression
//! and require a trailing `else -> body`.

use expo_ast::ast::{CondArm, Expr, ExprKind, MatchArm, Statement};
use expo_ast::token::TokenKind;

use crate::expr::BP_TERNARY;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_match_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // match

        let subject = self.parse_expr();
        self.skip_newlines();

        let arms = self.parse_until(|p| p.at(&TokenKind::End), |p| p.parse_match_arm(&[]));
        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::Match {
                subject: Box::new(subject),
                arms,
            },
            self.span_from(start),
        )
    }

    fn parse_match_arm(&mut self, extra_stops: &[TokenKind]) -> MatchArm {
        let start = self.current_span();
        let pattern = self.parse_or_pattern();

        let guard = if self.eat(&TokenKind::When).is_some() {
            Some(self.parse_expr_bp(BP_TERNARY + 1))
        } else {
            None
        };

        self.expect(&TokenKind::Arrow);
        let body = self.parse_match_body(extra_stops);

        MatchArm {
            pattern,
            guard,
            body,
            span: self.span_from(start),
        }
    }

    fn parse_match_body(&mut self, extra_stops: &[TokenKind]) -> Vec<Statement> {
        let mut stmts = Vec::new();

        let peek = self.peek();
        if !matches!(
            peek,
            TokenKind::End | TokenKind::EndOfFile | TokenKind::Newline
        ) && !extra_stops.contains(peek)
        {
            stmts.push(self.parse_statement());
        }

        self.skip_newlines();

        while !self.at(&TokenKind::End) && !extra_stops.iter().any(|t| self.at(t)) && !self.at_eof()
        {
            if self.looks_like_new_arm() {
                break;
            }
            let before = self.pos;
            stmts.push(self.parse_statement());
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }

        stmts
    }

    /// Heuristic: does the current position look like the start of a
    /// new match / cond / receive arm rather than a continuation
    /// statement? We scan forward to see if there's a `->` before a
    /// newline / `end`. Bracketed expressions (`(...)`, `{...}`,
    /// `[...]`) are skipped over by depth tracking.
    fn looks_like_new_arm(&self) -> bool {
        let mut i = self.pos;
        let mut depth = 0u32;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::Arrow if depth == 0 => return true,
                TokenKind::Newline | TokenKind::End | TokenKind::EndOfFile if depth == 0 => {
                    return false;
                }
                TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => {
                    depth += 1;
                }
                TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    pub(crate) fn parse_cond_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // cond
        self.skip_newlines();

        let arms = self.parse_until(
            |p| p.at(&TokenKind::End) || p.at(&TokenKind::Else),
            Self::parse_cond_arm,
        );
        let else_body = if self.eat(&TokenKind::Else).is_some() {
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body(&[]);
            self.skip_newlines();
            Some(body)
        } else {
            self.error("cond requires an `else ->` arm".into(), self.current_span());
            None
        };
        self.expect(&TokenKind::End);

        Expr::new(ExprKind::Cond { arms, else_body }, self.span_from(start))
    }

    fn parse_cond_arm(&mut self) -> CondArm {
        let arm_start = self.current_span();
        let condition = self.parse_expr_bp(crate::expr::BP_ARROW + 1);
        self.expect(&TokenKind::Arrow);
        let body = self.parse_match_body(&[]);
        CondArm {
            condition,
            body,
            span: self.span_from(arm_start),
        }
    }

    pub(crate) fn parse_receive_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // receive
        self.skip_newlines();

        let arms = self.parse_until(
            |p| p.at(&TokenKind::End) || p.at(&TokenKind::After),
            |p| p.parse_match_arm(&[TokenKind::After]),
        );

        let mut after_timeout = None;
        let mut after_body = Vec::new();
        if self.eat(&TokenKind::After).is_some() {
            after_timeout = Some(Box::new(self.parse_expr()));
            after_body = self.parse_block();
        }

        self.expect(&TokenKind::End);

        Expr::new(
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            },
            self.span_from(start),
        )
    }
}
