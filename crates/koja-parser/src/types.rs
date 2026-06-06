//! Type-expression parser. Handles the eight surface shapes:
//!
//! - `Int`, `String` (`Named` with a single segment)
//! - `Pkg.Type` (`Named` with a dotted path; packages are PascalCase)
//! - `List<Int>`, `Pkg.Container<T>` (`Generic`)
//! - `()` (`Unit`)
//! - `fn (A, B) -> C` (`Function`)
//! - `Self` (`Self_`)
//! - `A | B | C` (`Union`)
//!
//! Lowercase primitive aliases (`bool`, `i32`, …) used to be
//! accepted as a backwards-compatible bridge; they were removed
//! once the canonical PascalCase forms (`Bool`, `Int32`) landed in
//! stdlib and project sources alike.

use koja_ast::ast::TypeExpr;
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use crate::parser::{ERROR_IDENT, Parser};

impl Parser {
    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        let first = self.parse_primary_type_expr();
        if !self.at(&TokenKind::Pipe) {
            return first;
        }
        let start_span = type_expr_span(&first);
        let mut types = vec![first];
        while self.eat(&TokenKind::Pipe).is_some() {
            types.push(self.parse_primary_type_expr());
        }
        TypeExpr::Union {
            types,
            span: self.span_from(start_span),
        }
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
            TokenKind::TypeIdent(_) | TokenKind::Ident(_) => self.parse_dotted_type_path(),
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected type expression, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                TypeExpr::Named {
                    path: vec![ERROR_IDENT.to_string()],
                    span,
                }
            }
        }
    }

    fn parse_function_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        self.advance(); // fn
        self.expect(&TokenKind::LParen);

        let params = self.comma_separated(&TokenKind::RParen, Self::parse_type_expr);
        self.expect(&TokenKind::RParen);
        self.expect(&TokenKind::Arrow);
        let return_type = self.parse_type_expr();

        TypeExpr::Function {
            params,
            return_type: Box::new(return_type),
            span: self.span_from(start),
        }
    }

    fn parse_paren_type(&mut self) -> TypeExpr {
        let start = self.current_span();
        self.advance(); // (
        self.skip_newlines();
        if self.eat(&TokenKind::RParen).is_some() {
            return TypeExpr::Unit {
                span: self.span_from(start),
            };
        }

        let first = self.parse_type_expr();
        self.skip_newlines();
        self.expect(&TokenKind::RParen);
        first
    }

    /// Parse a possibly-dotted type path. Both segments are
    /// canonically `TypeIdent` (e.g. `JSON.Decoder`) since
    /// packages adopted PascalCase, but the parser also accepts a
    /// leading `Ident.` for legacy / mistaken inputs so the
    /// resolver can produce a "package names are PascalCase"
    /// diagnostic later instead of bailing here.
    ///
    /// Wraps the resulting path in `Generic` if a `<...>` argument
    /// list follows; otherwise returns `Named`.
    fn parse_dotted_type_path(&mut self) -> TypeExpr {
        let start = self.current_span();
        let mut path = Vec::new();

        loop {
            match self.peek().clone() {
                TokenKind::Ident(_) => path.push(self.expect_ident()),
                TokenKind::TypeIdent(_) => {
                    path.push(self.expect_type_ident());
                    while self.eat(&TokenKind::Dot).is_some() {
                        if matches!(self.peek(), TokenKind::TypeIdent(_)) {
                            path.push(self.expect_type_ident());
                        } else {
                            break;
                        }
                    }
                    return self.parse_optional_generic_args(path, start);
                }
                _ => break,
            }
            if self.eat(&TokenKind::Dot).is_none() {
                return TypeExpr::Named {
                    path,
                    span: self.span_from(start),
                };
            }
        }

        self.parse_optional_generic_args(path, start)
    }

    fn parse_optional_generic_args(&mut self, path: Vec<String>, start: Span) -> TypeExpr {
        if self.eat(&TokenKind::Lt).is_none() {
            return TypeExpr::Named {
                path,
                span: self.span_from(start),
            };
        }
        let mut args = vec![self.parse_type_expr()];
        while self.eat(&TokenKind::Comma).is_some() {
            args.push(self.parse_type_expr());
        }
        self.expect_gt();
        // Note: we don't route this through `comma_separated`
        // because the closing token here (`Gt` or the `>>`
        // ambiguity) needs the special `expect_gt` handling.
        TypeExpr::Generic {
            path,
            args,
            span: self.span_from(start),
        }
    }
}

fn type_expr_span(t: &TypeExpr) -> Span {
    match t {
        TypeExpr::Named { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Unit { span, .. }
        | TypeExpr::Function { span, .. }
        | TypeExpr::Self_ { span, .. }
        | TypeExpr::Union { span, .. } => *span,
    }
}
