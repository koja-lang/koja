use koja_ast::ast::{Arg, BinOp, Expr, ExprKind, Literal, UnaryOp};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use crate::parser::Parser;

// =========================================================================
// Binding powers for Pratt parsing
// =========================================================================

pub(crate) const BP_ARROW: u8 = 1;
pub(crate) const BP_TERNARY: u8 = 3;
pub(crate) const BP_OR_L: u8 = 6;
pub(crate) const BP_OR_R: u8 = 7;
pub(crate) const BP_AND_L: u8 = 8;
pub(crate) const BP_AND_R: u8 = 9;
pub(crate) const BP_NOT_R: u8 = 9;
pub(crate) const BP_CMP_L: u8 = 10;
pub(crate) const BP_CMP_R: u8 = 11;
pub(crate) const BP_ADD_L: u8 = 12;
pub(crate) const BP_ADD_R: u8 = 13;
pub(crate) const BP_MUL_L: u8 = 14;
pub(crate) const BP_MUL_R: u8 = 15;
pub(crate) const BP_UNARY_R: u8 = 17;
pub(crate) const BP_POSTFIX: u8 = 18;

fn infix_bp(kind: &TokenKind) -> Option<(u8, u8)> {
    match kind {
        TokenKind::Ident(name) if name == "or" => Some((BP_OR_L, BP_OR_R)),
        TokenKind::Ident(name) if name == "and" => Some((BP_AND_L, BP_AND_R)),
        TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::LtEq
        | TokenKind::GtEq => Some((BP_CMP_L, BP_CMP_R)),
        TokenKind::Plus | TokenKind::Minus | TokenKind::LtGt => Some((BP_ADD_L, BP_ADD_R)),
        TokenKind::Star | TokenKind::Slash | TokenKind::Percent => Some((BP_MUL_L, BP_MUL_R)),
        _ => None,
    }
}

fn token_to_binop(kind: &TokenKind) -> BinOp {
    match kind {
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Mod,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::NotEq => BinOp::NotEq,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::LtEq => BinOp::LtEq,
        TokenKind::GtEq => BinOp::GtEq,
        TokenKind::Ident(name) if name == "and" => BinOp::And,
        TokenKind::Ident(name) if name == "or" => BinOp::Or,
        TokenKind::LtGt => BinOp::Concat,
        _ => unreachable!("not a binary operator: {:?}", kind),
    }
}

// =========================================================================
// Core Pratt parser
// =========================================================================

impl Parser {
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    pub(crate) fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        self.parse_expr_bp_with_short_closure(min_bp, false)
    }

    fn parse_expr_bp_with_short_closure(&mut self, min_bp: u8, allow_short_closure: bool) -> Expr {
        let mut lhs = self.parse_prefix();

        loop {
            if matches!(self.peek(), TokenKind::Arrow) && min_bp <= BP_ARROW {
                lhs = self.parse_short_closure_tail(lhs, allow_short_closure);
                continue;
            }

            if BP_POSTFIX >= min_bp && self.postfix_at(min_bp) {
                lhs = self.parse_postfix(lhs, min_bp);
                continue;
            }

            if self.consume_continuation_newlines(min_bp) {
                continue;
            }

            if let Some((l_bp, r_bp)) = infix_bp(self.peek()) {
                if l_bp < min_bp {
                    break;
                }
                let op_token = self.advance();
                let op = token_to_binop(&op_token.kind);
                let rhs = self.parse_expr_bp(r_bp);
                let span = Span::new(lhs.span.start, rhs.span.end);
                lhs = Expr::new(
                    ExprKind::Binary {
                        op,
                        left: Box::new(lhs),
                        right: Box::new(rhs),
                    },
                    span,
                );
                continue;
            }

            break;
        }

        lhs
    }

    fn consume_continuation_newlines(&mut self, min_bp: u8) -> bool {
        if !matches!(self.peek(), TokenKind::Newline) {
            return false;
        }

        let saved = self.save_pos();
        self.skip_newlines();
        let continues = match self.peek() {
            TokenKind::Question => BP_TERNARY >= min_bp,
            kind @ TokenKind::Ident(name) if name == "and" || name == "or" => {
                infix_bp(kind).is_some_and(|(left_bp, _)| left_bp >= min_bp)
            }
            _ => false,
        };
        if !continues {
            self.restore_pos(saved);
        }
        continues
    }

    fn parse_call_postfix(&mut self, callee: Expr) -> Expr {
        self.advance(); // (
        let args = self.parse_arg_list();
        self.expect(&TokenKind::RParen);
        let span = Span::new(callee.span.start, self.prev_end());
        Expr::new(
            ExprKind::Call {
                callee: Box::new(callee),
                args,
                type_args: Vec::new(),
            },
            span,
        )
    }

    fn parse_dot_postfix(&mut self, receiver: Expr) -> Expr {
        self.advance(); // .
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                if self.at(&TokenKind::LParen) {
                    self.advance(); // (
                    let args = self.parse_arg_list();
                    self.expect(&TokenKind::RParen);
                    let span = Span::new(receiver.span.start, self.prev_end());
                    Expr::new(
                        ExprKind::MethodCall {
                            receiver: Box::new(receiver),
                            method: name,
                            args,
                            type_args: Vec::new(),
                        },
                        span,
                    )
                } else {
                    let span = Span::new(receiver.span.start, self.prev_end());
                    Expr::new(
                        ExprKind::FieldAccess {
                            receiver: Box::new(receiver),
                            field: name,
                        },
                        span,
                    )
                }
            }
            TokenKind::TypeIdent(variant) => {
                self.advance();
                let type_path = self.extract_type_path(&receiver);
                self.parse_enum_construction_tail(type_path, variant, receiver.span)
            }
            _ => {
                let span = Span::new(receiver.span.start, self.prev_end());
                self.error("expected field name or method after '.'".into(), span);
                Expr::new(
                    ExprKind::FieldAccess {
                        receiver: Box::new(receiver),
                        field: String::new(),
                    },
                    span,
                )
            }
        }
    }

    fn parse_postfix(&mut self, lhs: Expr, min_bp: u8) -> Expr {
        match self.peek() {
            TokenKind::Dot => self.parse_dot_postfix(lhs),
            TokenKind::LParen => self.parse_call_postfix(lhs),
            TokenKind::Question if BP_TERNARY >= min_bp => self.parse_ternary_tail(lhs),
            token => unreachable!("not a postfix token: {token:?}"),
        }
    }

    fn parse_short_closure_tail(&mut self, lhs: Expr, allowed: bool) -> Expr {
        if !allowed {
            self.error_with_hint(
                "short closures are only allowed as call arguments".into(),
                "use `fn (...) -> ... end` outside a call argument".into(),
                self.current_span(),
            );
        }
        self.advance(); // ->
        let params = self.expr_to_closure_params(&lhs, lhs.span);
        let body = self.parse_expr_bp(BP_ARROW);
        let span = Span::new(lhs.span.start, body.span.end);
        Expr::new(
            ExprKind::ShortClosure {
                params,
                body: Box::new(body),
            },
            span,
        )
    }

    fn parse_ternary_tail(&mut self, condition: Expr) -> Expr {
        let question_span = self.current_span();
        self.reject_nested_ternary(&condition, question_span);
        self.advance(); // ?
        self.skip_newlines();
        let then_expr = self.parse_expr_bp(0);
        self.reject_nested_ternary(&then_expr, then_expr.span);
        self.skip_newlines();
        self.expect(&TokenKind::Colon);
        let else_expr = self.parse_expr_bp(BP_TERNARY + 1);
        self.reject_nested_ternary(&else_expr, else_expr.span);
        let span = Span::new(condition.span.start, else_expr.span.end);
        Expr::new(
            ExprKind::Ternary {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            },
            span,
        )
    }

    fn postfix_at(&self, min_bp: u8) -> bool {
        matches!(self.peek(), TokenKind::Dot | TokenKind::LParen)
            || matches!(self.peek(), TokenKind::Question) && BP_TERNARY >= min_bp
    }

    fn reject_nested_ternary(&mut self, expr: &Expr, span: Span) {
        if matches!(expr.kind, ExprKind::Ternary { .. }) {
            self.error(
                "nested ternary not allowed, use `cond` instead".into(),
                span,
            );
        }
    }

    // =========================================================================
    // Prefix dispatch
    // =========================================================================

    fn parse_prefix(&mut self) -> Expr {
        match self.peek().clone() {
            TokenKind::Break => self.parse_break_recovery(),
            TokenKind::Cond => self.parse_cond_expr(),
            TokenKind::False => self.parse_literal_prefix(Literal::Bool(false)),
            TokenKind::FloatLit(text) => self.parse_literal_prefix(Literal::Float(text)),
            TokenKind::Fn => self.parse_closure_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::Ident(name) => self.parse_identifier_prefix(name),
            TokenKind::If => self.parse_if_expr(),
            TokenKind::IntLit(text) => self.parse_literal_prefix(Literal::Int(text)),
            TokenKind::LBracket => self.parse_list_expr(),
            TokenKind::LParen => self.parse_paren_expr(),
            TokenKind::Loop => self.parse_loop_expr(),
            TokenKind::LtLt => self.parse_binary_literal(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Minus => self.parse_unary_prefix(UnaryOp::Neg, BP_UNARY_R),
            TokenKind::MultilineStringStart => self.parse_string_expr(true),
            TokenKind::Not => self.parse_unary_prefix(UnaryOp::Not, BP_NOT_R),
            TokenKind::Receive => self.parse_receive_expr(),
            TokenKind::Return => self.parse_return_recovery(),
            TokenKind::Self_ => self.parse_self_prefix(),
            TokenKind::Spawn => self.parse_spawn_prefix(),
            TokenKind::StringStart => self.parse_string_expr(false),
            TokenKind::True => self.parse_literal_prefix(Literal::Bool(true)),
            TokenKind::TypeIdent(_) => self.parse_type_construction(),
            TokenKind::Unless => self.parse_unless_expr(),
            TokenKind::While => self.parse_while_expr(),
            _ => self.parse_unknown_prefix(),
        }
    }

    fn parse_break_recovery(&mut self) -> Expr {
        let start = self.current_span();
        self.error_with_hint(
            "`break` is only valid as a statement".into(),
            "put `break` on its own line inside a loop body".into(),
            start,
        );
        self.advance();
        Expr::new(
            ExprKind::Literal {
                value: Literal::Unit,
            },
            self.span_from(start),
        )
    }

    fn parse_identifier_prefix(&mut self, name: String) -> Expr {
        let start = self.current_span();
        self.advance();
        Expr::new(
            ExprKind::Ident {
                name,
                resolution: Resolution::Unresolved,
            },
            self.span_from(start),
        )
    }

    fn parse_literal_prefix(&mut self, value: Literal) -> Expr {
        let start = self.current_span();
        self.advance();
        Expr::new(ExprKind::Literal { value }, self.span_from(start))
    }

    fn parse_return_recovery(&mut self) -> Expr {
        let start = self.current_span();
        self.error_with_hint(
            "`return` is only valid as a statement".into(),
            "put `return` on its own line inside a function or closure body".into(),
            start,
        );
        self.advance();
        if matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::End | TokenKind::EndOfFile | TokenKind::RParen
        ) {
            return Expr::new(
                ExprKind::Literal {
                    value: Literal::Unit,
                },
                self.span_from(start),
            );
        }
        self.parse_expr()
    }

    fn parse_self_prefix(&mut self) -> Expr {
        let start = self.current_span();
        self.advance();
        Expr::new(ExprKind::Self_ { local_id: None }, self.span_from(start))
    }

    fn parse_spawn_prefix(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // spawn
        let expr = self.parse_expr();
        Expr::new(
            ExprKind::Spawn {
                expr: Box::new(expr),
            },
            self.span_from(start),
        )
    }

    fn parse_unary_prefix(&mut self, op: UnaryOp, binding_power: u8) -> Expr {
        let start = self.current_span();
        self.advance();
        let operand = self.parse_expr_bp(binding_power);
        Expr::new(
            ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            self.span_from(start),
        )
    }

    fn parse_unknown_prefix(&mut self) -> Expr {
        let span = self.current_span();
        self.error(format!("expected expression, found {}", self.peek()), span);
        self.advance();
        Expr::new(
            ExprKind::Literal {
                value: Literal::Unit,
            },
            span,
        )
    }

    // =========================================================================
    // Argument list
    // =========================================================================

    pub(crate) fn parse_arg_list(&mut self) -> Vec<Arg> {
        self.comma_separated(&TokenKind::RParen, Self::parse_arg)
    }

    fn parse_arg(&mut self) -> Arg {
        let start = self.current_span();

        if let TokenKind::Ident(name) = self.peek().clone()
            && matches!(self.peek_nth(1), TokenKind::Colon)
        {
            self.advance(); // ident
            self.advance(); // :
            let value = self.parse_expr_bp_with_short_closure(0, true);
            return Arg {
                name: Some(name),
                value,
                span: self.span_from(start),
            };
        }

        let value = self.parse_expr_bp_with_short_closure(0, true);
        Arg {
            name: None,
            value,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_literal_value(&mut self) -> Literal {
        match self.peek().clone() {
            TokenKind::IntLit(s) => {
                self.advance();
                Literal::Int(s)
            }
            TokenKind::FloatLit(s) => {
                self.advance();
                Literal::Float(s)
            }
            TokenKind::True => {
                self.advance();
                Literal::Bool(true)
            }
            TokenKind::False => {
                self.advance();
                Literal::Bool(false)
            }
            _ => {
                self.error(
                    format!("expected literal, found {}", self.peek()),
                    self.current_span(),
                );
                Literal::Unit
            }
        }
    }
}
