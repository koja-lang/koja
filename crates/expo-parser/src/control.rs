use expo_ast::ast::*;
use expo_ast::token::TokenKind;

use crate::expr::BP_TERNARY;
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

    pub(crate) fn parse_match_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // match

        let subject = self.parse_expr();
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            arms.push(self.parse_match_arm(&[]));
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }
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

    /// Heuristic: does the current position look like the start of a new
    /// match/cond/receive arm rather than a continuation statement?
    /// We scan forward to see if there's a `->` before a newline/end.
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

        let mut arms = Vec::new();
        let mut else_body = None;
        while !self.at(&TokenKind::End) && !self.at(&TokenKind::Else) && !self.at_eof() {
            let before = self.pos;
            let arm_start = self.current_span();
            let condition = self.parse_expr_bp(crate::expr::BP_ARROW + 1);
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body(&[]);
            arms.push(CondArm {
                condition,
                body,
                span: self.span_from(arm_start),
            });
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }
        if self.eat(&TokenKind::Else).is_some() {
            self.expect(&TokenKind::Arrow);
            else_body = Some(self.parse_match_body(&[]));
            self.skip_newlines();
        } else {
            self.error("cond requires an `else ->` arm".into(), self.current_span());
        }
        self.expect(&TokenKind::End);

        Expr::new(ExprKind::Cond { arms, else_body }, self.span_from(start))
    }

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

    pub(crate) fn parse_arena_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // arena
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::new(ExprKind::Arena { body }, self.span_from(start))
    }

    pub(crate) fn parse_receive_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // receive
        self.skip_newlines();

        let mut arms = Vec::new();
        let after_stop = [TokenKind::After];
        while !self.at(&TokenKind::End) && !self.at(&TokenKind::After) && !self.at_eof() {
            let before = self.pos;
            arms.push(self.parse_match_arm(&after_stop));
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }

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
