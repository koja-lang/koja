use expo_ast::ast::TypeExpr;
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        match self.peek().clone() {
            TokenKind::Ref => self.parse_ref_type(),
            TokenKind::LParen => self.parse_paren_type(),
            TokenKind::TypeIdent(_) => self.parse_named_type(),
            TokenKind::Ident(ref name) if is_primitive(name) => self.parse_primitive_type(),
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

    fn parse_ref_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        self.advance(); // ref
        self.expect(&TokenKind::Lt);
        let inner = self.parse_type_expr();
        self.expect(&TokenKind::Gt);
        TypeExpr::Ref {
            inner: Box::new(inner),
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

fn is_primitive(name: &str) -> bool {
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
