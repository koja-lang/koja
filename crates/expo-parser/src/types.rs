use expo_ast::ast::{PassMode, TypeExpr};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        let first = self.parse_primary_type_expr();
        if self.at(&TokenKind::Pipe) {
            let start_span = match &first {
                TypeExpr::Named { span, .. }
                | TypeExpr::Generic { span, .. }
                | TypeExpr::Tuple { span, .. }
                | TypeExpr::Unit { span, .. }
                | TypeExpr::Function { span, .. }
                | TypeExpr::Self_ { span, .. }
                | TypeExpr::Union { span, .. } => *span,
            };
            let mut types = vec![first];
            while self.eat(&TokenKind::Pipe).is_some() {
                types.push(self.parse_primary_type_expr());
            }
            return TypeExpr::Union {
                types,
                span: self.span_from(start_span),
            };
        }
        first
    }

    fn parse_primary_type_expr(&mut self) -> TypeExpr {
        match self.peek().clone() {
            TokenKind::Fn => self.parse_function_type(),
            TokenKind::LParen => self.parse_paren_type(),
            TokenKind::TypeIdent(ref name) if name == "Self" => {
                let span = self.current_span();
                self.advance();
                TypeExpr::Self_ {
                    span: self.span_from(span),
                }
            }
            TokenKind::TypeIdent(_) => self.parse_named_type(),
            TokenKind::Ident(ref name) if is_legacy_primitive(name) => self.parse_primitive_type(),
            TokenKind::Ident(_) if self.is_module_type_path() => self.parse_module_qualified_type(),
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected type expression, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                TypeExpr::Named {
                    path: vec!["<error>".to_string()],
                    span,
                }
            }
        }
    }

    /// Lookahead check: is this `ident.` followed eventually by a TypeIdent?
    fn is_module_type_path(&self) -> bool {
        matches!(self.peek_nth(1), TokenKind::Dot)
    }

    fn parse_function_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        self.advance(); // fn
        self.expect(&TokenKind::LParen);

        let mut params = Vec::new();
        let mut param_modes = Vec::new();
        if !self.at(&TokenKind::RParen) {
            param_modes.push(if self.eat(&TokenKind::Move).is_some() {
                PassMode::Move
            } else {
                PassMode::Borrow
            });
            params.push(self.parse_type_expr());
            while self.eat(&TokenKind::Comma).is_some() {
                if self.at(&TokenKind::RParen) {
                    break;
                }
                param_modes.push(if self.eat(&TokenKind::Move).is_some() {
                    PassMode::Move
                } else {
                    PassMode::Borrow
                });
                params.push(self.parse_type_expr());
            }
        }
        self.expect(&TokenKind::RParen);
        self.expect(&TokenKind::Arrow);
        let return_type = self.parse_type_expr();

        TypeExpr::Function {
            params,
            param_modes,
            return_type: Box::new(return_type),
            span: self.span_from(start),
        }
    }

    fn parse_paren_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        self.advance(); // (
        if self.eat(&TokenKind::RParen).is_some() {
            return TypeExpr::Unit {
                span: self.span_from(start),
            };
        }

        let first = self.parse_type_expr();
        if self.eat(&TokenKind::Comma).is_some() {
            let mut elements = vec![first];
            elements.push(self.parse_type_expr());
            while self.eat(&TokenKind::Comma).is_some() {
                if self.at(&TokenKind::RParen) {
                    break;
                }
                elements.push(self.parse_type_expr());
            }
            self.expect(&TokenKind::RParen);
            TypeExpr::Tuple {
                elements,
                span: self.span_from(start),
            }
        } else {
            self.expect(&TokenKind::RParen);
            first
        }
    }

    fn parse_named_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        let first = self.expect_type_ident();
        let mut path = vec![first];

        while self.eat(&TokenKind::Dot).is_some() {
            if matches!(self.peek(), TokenKind::TypeIdent(_)) {
                path.push(self.expect_type_ident());
            } else {
                break;
            }
        }

        self.parse_optional_generic_args(path, start)
    }

    fn parse_module_qualified_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        let mut path = Vec::new();

        while let TokenKind::Ident(_) = self.peek().clone() {
            let name = self.expect_ident();
            path.push(name);
            if self.eat(&TokenKind::Dot).is_none() {
                return TypeExpr::Named {
                    path,
                    span: self.span_from(start),
                };
            }
        }

        if matches!(self.peek(), TokenKind::TypeIdent(_)) {
            path.push(self.expect_type_ident());
            while self.eat(&TokenKind::Dot).is_some() {
                if matches!(self.peek(), TokenKind::TypeIdent(_)) {
                    path.push(self.expect_type_ident());
                } else {
                    break;
                }
            }
        }

        self.parse_optional_generic_args(path, start)
    }

    fn parse_optional_generic_args(
        &mut self,
        path: Vec<String>,
        start: expo_ast::span::Span,
    ) -> TypeExpr {
        if self.eat(&TokenKind::Lt).is_some() {
            let mut args = vec![self.parse_type_expr()];
            while self.eat(&TokenKind::Comma).is_some() {
                args.push(self.parse_type_expr());
            }
            self.expect(&TokenKind::Gt);
            TypeExpr::Generic {
                path,
                args,
                span: self.span_from(start),
            }
        } else {
            TypeExpr::Named {
                path,
                span: self.span_from(start),
            }
        }
    }

    fn parse_primitive_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        let name = self.expect_ident();
        TypeExpr::Named {
            path: vec![name],
            span: self.span_from(start),
        }
    }
}

/// Recognizes old lowercase primitive names for backward compatibility during
/// the transition period. New code should use PascalCase (Int, String, Bool, etc.)
/// which lex as TypeIdent and flow through parse_named_type automatically.
fn is_legacy_primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "string"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
    )
}
