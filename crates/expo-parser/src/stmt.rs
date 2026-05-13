use expo_ast::ast::*;
use expo_ast::token::TokenKind;

use crate::expr::expr_span;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_statement(&mut self) -> Statement {
        match self.peek() {
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => self.parse_break(),
            TokenKind::Const => {
                let span = self.current_span();
                self.error_with_hint(
                    "constants must be declared at the module level".to_string(),
                    "move this `const` outside of the function body".into(),
                    span,
                );
                self.advance();
                self.parse_expr_or_assign()
            }
            _ => self.parse_expr_or_assign(),
        }
    }

    fn parse_return(&mut self) -> Statement {
        let start = self.current_span();
        self.advance(); // return

        let value = if matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::End | TokenKind::EndOfFile
        ) {
            None
        } else {
            Some(self.parse_expr())
        };

        Statement::Return {
            value,
            span: self.span_from(start),
        }
    }

    fn parse_break(&mut self) -> Statement {
        let start = self.current_span();
        self.advance(); // break
        Statement::Break {
            span: self.span_from(start),
        }
    }

    fn parse_expr_or_assign(&mut self) -> Statement {
        let expr = self.parse_expr();
        let start_span = expr_span(&expr);

        match self.peek() {
            TokenKind::Colon if matches!(&expr.kind, ExprKind::Ident { .. }) => {
                self.advance();
                let type_annotation = self.parse_type_expr();
                self.expect(&TokenKind::Eq);
                let value = self.parse_expr();
                let span = self.span_from(start_span);

                let name = if let ExprKind::Ident { name, .. } = &expr.kind {
                    name.clone()
                } else {
                    unreachable!()
                };

                Statement::Assignment {
                    target: AssignTarget::LValue(LValue {
                        head_resolved_type: None,
                        local_id: None,
                        segments: vec![name],
                        span: start_span,
                    }),
                    type_annotation: Some(type_annotation),
                    value,
                    span,
                }
            }
            TokenKind::Eq => {
                self.advance();
                let value = self.parse_expr();
                let span = self.span_from(start_span);

                let target = if let Some(lvalue) = try_expr_to_lvalue(&expr) {
                    AssignTarget::LValue(lvalue)
                } else if let Some(pattern) = self.try_expr_to_pattern(&expr) {
                    AssignTarget::Pattern(pattern)
                } else {
                    self.error_with_hint(
                        "invalid assignment target".to_string(),
                        "only variables and fields can be assigned to".into(),
                        start_span,
                    );
                    AssignTarget::LValue(LValue {
                        head_resolved_type: None,
                        local_id: None,
                        segments: vec!["<error>".to_string()],
                        span: start_span,
                    })
                };

                Statement::Assignment {
                    target,
                    type_annotation: None,
                    value,
                    span,
                }
            }
            TokenKind::PlusEq | TokenKind::MinusEq | TokenKind::StarEq | TokenKind::SlashEq => {
                let op_token = self.advance();
                let op = match op_token.kind {
                    TokenKind::PlusEq => CompoundOp::Add,
                    TokenKind::MinusEq => CompoundOp::Sub,
                    TokenKind::StarEq => CompoundOp::Mul,
                    TokenKind::SlashEq => CompoundOp::Div,
                    _ => unreachable!(),
                };
                let value = self.parse_expr();
                let span = self.span_from(start_span);

                let target = if let Some(lvalue) = try_expr_to_lvalue(&expr) {
                    lvalue
                } else {
                    self.error_with_hint(
                        "invalid compound assignment target".to_string(),
                        "only variables and fields can be assigned to".into(),
                        start_span,
                    );
                    LValue {
                        head_resolved_type: None,
                        local_id: None,
                        segments: vec!["<error>".to_string()],
                        span: start_span,
                    }
                };

                Statement::CompoundAssign {
                    target,
                    op,
                    value,
                    span,
                }
            }
            _ => Statement::Expr(expr),
        }
    }

    fn try_expr_to_pattern(&mut self, expr: &Expr) -> Option<Pattern> {
        match &expr.kind {
            ExprKind::Ident { name, .. } if name == "_" => {
                Some(Pattern::Wildcard { span: expr.span })
            }
            ExprKind::Ident { name, .. } => Some(Pattern::Binding {
                local_id: None,
                name: name.clone(),
                span: expr.span,
            }),
            _ => None,
        }
    }
}

fn try_expr_to_lvalue(expr: &Expr) -> Option<LValue> {
    match &expr.kind {
        ExprKind::Ident { name, .. } => Some(LValue {
            head_resolved_type: None,
            local_id: None,
            segments: vec![name.clone()],
            span: expr.span,
        }),
        ExprKind::Self_ { .. } => Some(LValue {
            head_resolved_type: None,
            local_id: None,
            segments: vec!["self".to_string()],
            span: expr.span,
        }),
        ExprKind::FieldAccess {
            receiver, field, ..
        } => {
            let mut lvalue = try_expr_to_lvalue(receiver)?;
            lvalue.segments.push(field.clone());
            lvalue.span = expr.span;
            Some(lvalue)
        }
        _ => None,
    }
}
