use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_ast::token::TokenKind;

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
        TokenKind::Or => Some((BP_OR_L, BP_OR_R)),
        TokenKind::And => Some((BP_AND_L, BP_AND_R)),
        TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::LtEq
        | TokenKind::GtEq => Some((BP_CMP_L, BP_CMP_R)),
        TokenKind::Plus | TokenKind::Minus => Some((BP_ADD_L, BP_ADD_R)),
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
        TokenKind::And => BinOp::And,
        TokenKind::Or => BinOp::Or,
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
                let params = self.expr_to_closure_params(&lhs, expr_span(&lhs));
                let body = self.parse_expr_bp(BP_ARROW);
                let span = Span::new(expr_span(&lhs).start, expr_span(&body).end);
                lhs = Expr::ShortClosure {
                    params,
                    body: Box::new(body),
                    span,
                };
                continue;
            }

            // Postfix operators (highest precedence)
            if BP_POSTFIX >= min_bp {
                match self.peek() {
                    TokenKind::Dot => {
                        self.advance(); // .
                        let keyword_as_ident = match self.peek() {
                            TokenKind::Or => Some("or".to_string()),
                            TokenKind::And => Some("and".to_string()),
                            _ => None,
                        };
                        match self.peek().clone() {
                            _ if keyword_as_ident.is_some() => {
                                let name = keyword_as_ident.unwrap();
                                self.advance();
                                if self.at(&TokenKind::LParen) {
                                    self.advance(); // (
                                    let args = self.parse_arg_list();
                                    self.expect(&TokenKind::RParen);
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::MethodCall {
                                        receiver: Box::new(lhs),
                                        method: name,
                                        args,
                                        span,
                                    };
                                } else {
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::FieldAccess {
                                        receiver: Box::new(lhs),
                                        field: name,
                                        span,
                                    };
                                }
                                continue;
                            }
                            TokenKind::Ident(name) => {
                                self.advance();
                                if self.at(&TokenKind::LParen) {
                                    self.advance(); // (
                                    let args = self.parse_arg_list();
                                    self.expect(&TokenKind::RParen);
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::MethodCall {
                                        receiver: Box::new(lhs),
                                        method: name,
                                        args,
                                        span,
                                    };
                                } else {
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::FieldAccess {
                                        receiver: Box::new(lhs),
                                        field: name,
                                        span,
                                    };
                                }
                                continue;
                            }
                            TokenKind::TypeIdent(variant) => {
                                self.advance();
                                let type_path = self.extract_type_path(&lhs);
                                lhs = self.parse_enum_construction_tail(
                                    type_path,
                                    variant,
                                    expr_span(&lhs),
                                );
                                continue;
                            }
                            _ => {
                                let span = self.current_span();
                                self.error(
                                    format!(
                                        "expected field name or variant after '.', found {:?}",
                                        self.peek()
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                    TokenKind::LParen => {
                        self.advance(); // (
                        let args = self.parse_arg_list();
                        self.expect(&TokenKind::RParen);
                        let span = Span::new(expr_span(&lhs).start, self.prev_end());
                        lhs = Expr::Call {
                            callee: Box::new(lhs),
                            args,
                            span,
                        };
                        continue;
                    }
                    TokenKind::Question if BP_TERNARY >= min_bp => {
                        if matches!(lhs, Expr::Ternary { .. }) {
                            let espan = self.current_span();
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                espan,
                            );
                        }
                        self.advance(); // ?
                        self.skip_newlines();
                        let then_expr = self.parse_expr_bp(0);
                        if matches!(then_expr, Expr::Ternary { .. }) {
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                expr_span(&then_expr),
                            );
                        }
                        self.skip_newlines();
                        self.expect(&TokenKind::Colon);
                        let else_expr = self.parse_expr_bp(BP_TERNARY + 1);
                        if matches!(else_expr, Expr::Ternary { .. }) {
                            self.error(
                                "nested ternary not allowed, use `cond` instead".into(),
                                expr_span(&else_expr),
                            );
                        }
                        let span = Span::new(expr_span(&lhs).start, expr_span(&else_expr).end);
                        lhs = Expr::Ternary {
                            condition: Box::new(lhs),
                            then_expr: Box::new(then_expr),
                            else_expr: Box::new(else_expr),
                            span,
                        };
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
                let span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
                lhs = Expr::Binary {
                    op,
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                    span,
                };
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
                Expr::Literal {
                    value: Literal::Int(s),
                    span: self.span_from(start),
                }
            }
            TokenKind::FloatLit(s) => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Float(s),
                    span: self.span_from(start),
                }
            }
            TokenKind::True => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Bool(true),
                    span: self.span_from(start),
                }
            }
            TokenKind::False => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Bool(false),
                    span: self.span_from(start),
                }
            }
            TokenKind::StringStart => self.parse_string_expr(false),
            TokenKind::MultilineStringStart => self.parse_string_expr(true),

            TokenKind::Ident(name) => {
                let start = self.current_span();
                self.advance();
                Expr::Ident {
                    name,
                    span: self.span_from(start),
                }
            }

            TokenKind::TypeIdent(_) => self.parse_type_construction(),

            TokenKind::Self_ => {
                let start = self.current_span();
                self.advance();
                Expr::Self_ {
                    span: self.span_from(start),
                }
            }

            TokenKind::LParen => self.parse_paren_expr(),
            TokenKind::LBracket => self.parse_list_expr(),
            TokenKind::LtLt => self.parse_binary_literal(),

            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_R);
                Expr::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                    span: self.span_from(start),
                }
            }

            TokenKind::Not => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_NOT_R);
                Expr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                    span: self.span_from(start),
                }
            }

            TokenKind::If => self.parse_if_expr(),
            TokenKind::Unless => self.parse_unless_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Cond => self.parse_cond_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::Loop => self.parse_loop_expr(),
            TokenKind::While => self.parse_while_expr(),
            TokenKind::Arena => self.parse_arena_expr(),
            TokenKind::Receive => self.parse_receive_expr(),
            TokenKind::Spawn => self.parse_spawn_expr(),
            TokenKind::Fn => self.parse_closure_expr(),

            TokenKind::Break => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Unit,
                    span: self.span_from(start),
                }
            }

            TokenKind::Return => {
                let start = self.current_span();
                self.advance();
                let value = if matches!(
                    self.peek(),
                    TokenKind::Newline | TokenKind::End | TokenKind::Eof | TokenKind::RParen
                ) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                if let Some(val) = value {
                    val
                } else {
                    Expr::Literal {
                        value: Literal::Unit,
                        span: self.span_from(start),
                    }
                }
            }

            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected expression, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                Expr::Literal {
                    value: Literal::Unit,
                    span,
                }
            }
        }
    }

    // =========================================================================
    // Argument list
    // =========================================================================

    pub(crate) fn parse_arg_list(&mut self) -> Vec<Arg> {
        let mut args = Vec::new();
        self.skip_newlines();
        if self.at(&TokenKind::RParen) {
            return args;
        }

        args.push(self.parse_arg());
        while self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            if self.at(&TokenKind::RParen) {
                break;
            }
            args.push(self.parse_arg());
        }
        self.skip_newlines();
        args
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
                    format!("expected literal, found {:?}", self.peek()),
                    self.current_span(),
                );
                Literal::Unit
            }
        }
    }
}

// =========================================================================
// Utility
// =========================================================================

pub(crate) fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Arena { span, .. }
        | Expr::Binary { span, .. }
        | Expr::BinaryLiteral { span, .. }
        | Expr::Call { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Cond { span, .. }
        | Expr::EnumConstruction { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::For { span, .. }
        | Expr::Group { span, .. }
        | Expr::Ident { span, .. }
        | Expr::If { span, .. }
        | Expr::List { span, .. }
        | Expr::Map { span, .. }
        | Expr::Literal { span, .. }
        | Expr::Loop { span, .. }
        | Expr::Match { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Receive { span, .. }
        | Expr::Self_ { span, .. }
        | Expr::ShortClosure { span, .. }
        | Expr::Spawn { span, .. }
        | Expr::String { span, .. }
        | Expr::StructConstruction { span, .. }
        | Expr::Ternary { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Unless { span, .. }
        | Expr::While { span, .. } => *span,
    }
}
