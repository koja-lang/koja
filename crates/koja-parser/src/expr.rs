use koja_ast::ast::*;
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
        let mut lhs = self.parse_prefix();

        loop {
            // Short closure arrow
            if matches!(self.peek(), TokenKind::Arrow) && min_bp <= BP_ARROW {
                self.advance(); // ->
                let params = self.expr_to_closure_params(&lhs, lhs.span);
                let body = self.parse_expr_bp(BP_ARROW);
                let span = Span::new(lhs.span.start, body.span.end);
                lhs = Expr::new(
                    ExprKind::ShortClosure {
                        params,
                        body: Box::new(body),
                    },
                    span,
                );
                continue;
            }

            // Postfix operators (highest precedence)
            if BP_POSTFIX >= min_bp {
                match self.peek() {
                    TokenKind::Dot => {
                        self.advance(); // .
                        match self.peek().clone() {
                            TokenKind::Ident(name) => {
                                self.advance();
                                if self.at(&TokenKind::LParen) {
                                    self.advance(); // (
                                    let args = self.parse_arg_list();
                                    self.expect(&TokenKind::RParen);
                                    let span = Span::new(lhs.span.start, self.prev_end());
                                    lhs = Expr::new(
                                        ExprKind::MethodCall {
                                            receiver: Box::new(lhs),
                                            method: name,
                                            args,
                                            type_args: Vec::new(),
                                        },
                                        span,
                                    );
                                } else {
                                    let span = Span::new(lhs.span.start, self.prev_end());
                                    lhs = Expr::new(
                                        ExprKind::FieldAccess {
                                            receiver: Box::new(lhs),
                                            field: name,
                                        },
                                        span,
                                    );
                                }
                                continue;
                            }
                            TokenKind::TypeIdent(variant) => {
                                self.advance();
                                let type_path = self.extract_type_path(&lhs);
                                lhs =
                                    self.parse_enum_construction_tail(type_path, variant, lhs.span);
                                continue;
                            }
                            _ => {
                                let span = Span::new(lhs.span.start, self.prev_end());
                                self.error("expected field name or method after '.'".into(), span);
                                lhs = Expr::new(
                                    ExprKind::FieldAccess {
                                        receiver: Box::new(lhs),
                                        field: String::new(),
                                    },
                                    span,
                                );
                                continue;
                            }
                        }
                    }
                    TokenKind::LParen => {
                        self.advance(); // (
                        let args = self.parse_arg_list();
                        self.expect(&TokenKind::RParen);
                        let span = Span::new(lhs.span.start, self.prev_end());
                        lhs = Expr::new(
                            ExprKind::Call {
                                callee: Box::new(lhs),
                                args,
                                type_args: Vec::new(),
                            },
                            span,
                        );
                        continue;
                    }
                    TokenKind::Question if BP_TERNARY >= min_bp => {
                        if matches!(lhs.kind, ExprKind::Ternary { .. }) {
                            let espan = self.current_span();
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                espan,
                            );
                        }
                        self.advance(); // ?
                        self.skip_newlines();
                        let then_expr = self.parse_expr_bp(0);
                        if matches!(then_expr.kind, ExprKind::Ternary { .. }) {
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                then_expr.span,
                            );
                        }
                        self.skip_newlines();
                        self.expect(&TokenKind::Colon);
                        let else_expr = self.parse_expr_bp(BP_TERNARY + 1);
                        if matches!(else_expr.kind, ExprKind::Ternary { .. }) {
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                else_expr.span,
                            );
                        }
                        let span = Span::new(lhs.span.start, else_expr.span.end);
                        lhs = Expr::new(
                            ExprKind::Ternary {
                                condition: Box::new(lhs),
                                then_expr: Box::new(then_expr),
                                else_expr: Box::new(else_expr),
                            },
                            span,
                        );
                        continue;
                    }
                    _ => {}
                }
            }

            // Ternary continuation across newlines: `expr\n  ? ...`
            if matches!(self.peek(), TokenKind::Newline) && BP_TERNARY >= min_bp {
                let saved = self.save_pos();
                self.skip_newlines();
                if matches!(self.peek(), TokenKind::Question) {
                    continue;
                }
                self.restore_pos(saved);
            }

            // Infix operators
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

    // =========================================================================
    // Prefix dispatch
    // =========================================================================

    fn parse_prefix(&mut self) -> Expr {
        match self.peek().clone() {
            TokenKind::IntLit(s) => {
                let start = self.current_span();
                self.advance();
                Expr::new(
                    ExprKind::Literal {
                        value: Literal::Int(s),
                    },
                    self.span_from(start),
                )
            }
            TokenKind::FloatLit(s) => {
                let start = self.current_span();
                self.advance();
                Expr::new(
                    ExprKind::Literal {
                        value: Literal::Float(s),
                    },
                    self.span_from(start),
                )
            }
            TokenKind::True => {
                let start = self.current_span();
                self.advance();
                Expr::new(
                    ExprKind::Literal {
                        value: Literal::Bool(true),
                    },
                    self.span_from(start),
                )
            }
            TokenKind::False => {
                let start = self.current_span();
                self.advance();
                Expr::new(
                    ExprKind::Literal {
                        value: Literal::Bool(false),
                    },
                    self.span_from(start),
                )
            }
            TokenKind::StringStart => self.parse_string_expr(false),
            TokenKind::MultilineStringStart => self.parse_string_expr(true),

            TokenKind::Ident(name) => {
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

            TokenKind::TypeIdent(_) => self.parse_type_construction(),

            TokenKind::Self_ => {
                let start = self.current_span();
                self.advance();
                Expr::new(ExprKind::Self_ { local_id: None }, self.span_from(start))
            }

            TokenKind::LParen => self.parse_paren_expr(),
            TokenKind::LBracket => self.parse_list_expr(),
            TokenKind::LtLt => self.parse_binary_literal(),

            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_R);
                Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Neg,
                        operand: Box::new(operand),
                    },
                    self.span_from(start),
                )
            }

            TokenKind::Not => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_NOT_R);
                Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        operand: Box::new(operand),
                    },
                    self.span_from(start),
                )
            }

            TokenKind::If => self.parse_if_expr(),
            TokenKind::Unless => self.parse_unless_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Cond => self.parse_cond_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::Loop => self.parse_loop_expr(),
            TokenKind::While => self.parse_while_expr(),
            TokenKind::Receive => self.parse_receive_expr(),
            TokenKind::Spawn => {
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
            TokenKind::Fn => self.parse_closure_expr(),

            TokenKind::Break => {
                let start = self.current_span();
                self.advance();
                Expr::new(
                    ExprKind::Literal {
                        value: Literal::Unit,
                    },
                    self.span_from(start),
                )
            }

            TokenKind::Return => {
                let start = self.current_span();
                self.advance();
                let value = if matches!(
                    self.peek(),
                    TokenKind::Newline | TokenKind::End | TokenKind::EndOfFile | TokenKind::RParen
                ) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                if let Some(val) = value {
                    val
                } else {
                    Expr::new(
                        ExprKind::Literal {
                            value: Literal::Unit,
                        },
                        self.span_from(start),
                    )
                }
            }

            _ => {
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
        }
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
            let value = self.parse_expr();
            return Arg {
                name: Some(name),
                value,
                span: self.span_from(start),
            };
        }

        let value = self.parse_expr();
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
