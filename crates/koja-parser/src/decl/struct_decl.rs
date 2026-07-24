//! `struct Name<...> ... end`.
//!
//! Struct bodies interleave field declarations with inline functions
//! and nested type declarations. The [`Parser::parse_type_body_member`]
//! helper is shared with `enum` and `impl` bodies.

use koja_ast::ast::{Annotation, Function, Item, StructDecl, StructField, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

/// A single non-field member of a `struct`, `enum`, or `impl` body.
pub(crate) enum TypeBodyMember {
    Function(Box<Function>),
    Nested(Box<Item>),
}

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
        let mut nested = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek().clone() {
                TokenKind::Fn
                | TokenKind::Priv
                | TokenKind::At
                | TokenKind::Struct
                | TokenKind::Enum => match self.parse_type_body_member("struct") {
                    TypeBodyMember::Function(function) => functions.push(*function),
                    TypeBodyMember::Nested(item) => nested.push(*item),
                },
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
            nested,
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

    /// Parse one member of a `struct`, `enum`, or `impl` body,
    /// handling the `priv` and `@annotation` prefixes uniformly.
    pub(crate) fn parse_type_body_member(&mut self, context: &str) -> TypeBodyMember {
        let annotations = if self.at(&TokenKind::At) {
            let annotations = self.parse_annotations();
            self.skip_newlines();
            annotations
        } else {
            Vec::new()
        };

        let visibility = if self.at(&TokenKind::Priv) {
            self.advance();
            Visibility::Private
        } else {
            Visibility::Public
        };

        match self.peek() {
            TokenKind::Struct | TokenKind::Enum => TypeBodyMember::Nested(Box::new(
                self.parse_nested_type_item(annotations, visibility),
            )),
            TokenKind::Fn => TypeBodyMember::Function(Box::new(
                self.parse_function_decl(annotations, visibility),
            )),
            _ => {
                let span = self.current_span();
                self.error(
                    format!(
                        "expected a function or type declaration in {context} block, found {}",
                        self.peek()
                    ),
                    span,
                );
                TypeBodyMember::Function(Box::new(
                    self.parse_function_decl(annotations, visibility),
                ))
            }
        }
    }

    /// Parse a `struct`/`enum` declared inside another type's body.
    fn parse_nested_type_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let keyword_span = self.current_span();
        let item = if self.at(&TokenKind::Struct) {
            self.parse_struct_item(annotations, visibility)
        } else {
            self.parse_enum_item(annotations, visibility)
        };
        let path = match &item {
            Item::Struct(decl) => &decl.path,
            Item::Enum(decl) => &decl.path,
            _ => unreachable!("nested type item is always a struct or enum"),
        };
        if path.len() > 1 {
            self.error(
                format!(
                    "nested type declarations take a single name, found `{}`. The enclosing type's prefix is implied",
                    path.join(".")
                ),
                keyword_span,
            );
        }
        item
    }
}
