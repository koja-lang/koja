use expo_ast::ast::*;
use expo_ast::token::TokenKind;

use crate::expr::BP_PIPE_L;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_if_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // if

        let condition = self.parse_expr();
        let then_body = self.parse_block();

        let else_body = if self.eat(&TokenKind::Else).is_some() {
            Some(self.parse_block())
        } else {
            None
        };
        self.expect(&TokenKind::End);

        Expr::If {
            condition: Box::new(condition),
            then_body,
            else_body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_unless_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // unless

        let condition = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Unless {
            condition: Box::new(condition),
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_match_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // match

        let subject = self.parse_expr();
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            arms.push(self.parse_match_arm());
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Match {
            subject: Box::new(subject),
            arms,
            span: self.span_from(start),
        }
    }

    fn parse_match_arm(&mut self) -> MatchArm {
        let start = self.current_span();
        let pattern = self.parse_pattern();

        let guard = if self.eat(&TokenKind::When).is_some() {
            Some(self.parse_expr_bp(BP_PIPE_L))
        } else {
            None
        };

        self.expect(&TokenKind::Arrow);
        let body = self.parse_match_body();

        MatchArm {
            pattern,
            guard,
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_match_body(&mut self) -> Vec<Statement> {
        let mut stmts = Vec::new();

        if !matches!(
            self.peek(),
            TokenKind::End | TokenKind::Eof | TokenKind::Newline
        ) {
            stmts.push(self.parse_statement());
        }

        self.skip_newlines();

        while !self.at(&TokenKind::End) && !self.at_eof() {
            if self.looks_like_new_arm() {
                break;
            }
            let before = self.pos;
            stmts.push(self.parse_statement());
            if self.pos == before {
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
                TokenKind::Newline | TokenKind::End | TokenKind::Eof if depth == 0 => {
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
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            let arm_start = self.current_span();
            let condition = self.parse_expr_bp(crate::expr::BP_ARROW + 1);
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body();
            arms.push(CondArm {
                condition,
                body,
                span: self.span_from(arm_start),
            });
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Cond {
            arms,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_for_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // for

        let pattern = self.parse_pattern();
        self.expect(&TokenKind::In);
        let iterable = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::For {
            pattern,
            iterable: Box::new(iterable),
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_loop_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // loop
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Loop {
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_while_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // while
        let condition = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::While {
            condition: Box::new(condition),
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_arena_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // arena
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Arena {
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_receive_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // receive
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            let arm_start = self.current_span();
            let pattern = self.parse_pattern();
            self.expect(&TokenKind::Eq);
            let source = self.parse_expr_bp(BP_PIPE_L);
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body();
            arms.push(ReceiveArm {
                pattern,
                source,
                body,
                span: self.span_from(arm_start),
            });
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Receive {
            arms,
            span: self.span_from(start),
        }
    }
}
