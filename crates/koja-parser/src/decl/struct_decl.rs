//! `struct Name<...> ... end`.
//!
//! Struct bodies interleave field declarations with inline `fn`,
//! `priv fn`, and `@annotated fn` method definitions. The
//! [`Parser::parse_type_body_function`] helper is shared with
//! `enum` and `impl` bodies which have the same three-way split.

use koja_ast::ast::{Annotation, Function, Item, StructDecl, StructField, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_struct_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // struct

        let path = self.parse_decl_path();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut fields = Vec::new();
        let mut functions = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    functions.push(self.parse_type_body_function("struct"));
                }
                _ => {
                    fields.push(self.parse_struct_field());
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Struct(StructDecl {
            annotations,
            visibility,
            path,
            type_params,
            fields,
            functions,
            span: self.span_from(start),
        })
    }

    pub(crate) fn parse_struct_field(&mut self) -> StructField {
        let start = self.current_span();
        let name = self.expect_ident();
        self.expect(&TokenKind::Colon);
        let type_expr = self.parse_type_expr();
        let default = if self.eat(&TokenKind::Eq).is_some() {
            Some(self.parse_expr())
        } else {
            None
        };
        StructField {
            name,
            type_expr,
            default,
            span: self.span_from(start),
        }
    }

    /// Parse a single function declaration sitting inside a `struct`,
    /// `enum`, or `impl` body. Handles `fn`, `priv fn`, and
    /// `@annotation fn` / `@annotation priv fn` uniformly.
    pub(crate) fn parse_type_body_function(&mut self, context: &str) -> Function {
        match self.peek().clone() {
            TokenKind::Fn => self.parse_function_decl(Vec::new(), Visibility::Public),
            TokenKind::Priv => {
                self.advance();
                self.parse_function_decl(Vec::new(), Visibility::Private)
            }
            TokenKind::At => {
                let annotations = self.parse_annotations();
                self.skip_newlines();
                match self.peek() {
                    TokenKind::Fn => self.parse_function_decl(annotations, Visibility::Public),
                    TokenKind::Priv => {
                        self.advance();
                        self.parse_function_decl(annotations, Visibility::Private)
                    }
                    _ => {
                        let span = self.current_span();
                        self.error(
                            format!("annotation in {context} block must be followed by a function"),
                            span,
                        );
                        self.parse_function_decl(annotations, Visibility::Public)
                    }
                }
            }
            _ => unreachable!("parse_type_body_function called on non-function token"),
        }
    }
}
