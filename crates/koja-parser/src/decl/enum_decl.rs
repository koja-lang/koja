//! `enum Name<...> ... end`.
//!
//! Enum bodies share the function/field-interleave shape with
//! `struct`: variant declarations can be intermixed with inline
//! `fn` / `priv fn` / `@annotation fn` definitions. Variants come in
//! three shapes: Unit (`Color`), Tuple (`Rect(Int, Int)`), and
//! Struct (`Pixel { x: Int, y: Int }`).

use koja_ast::ast::{Annotation, EnumDecl, EnumVariant, EnumVariantData, Item, Visibility};
use koja_ast::token::TokenKind;

use crate::decl::struct_decl::TypeBodyMember;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_enum_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // enum

        let path = self.parse_decl_path();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut variants = Vec::new();
        let mut functions = Vec::new();
        let mut nested = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek().clone() {
                TokenKind::Fn
                | TokenKind::Priv
                | TokenKind::At
                | TokenKind::Struct
                | TokenKind::Enum => match self.parse_type_body_member("enum") {
                    TypeBodyMember::Function(function) => functions.push(*function),
                    TypeBodyMember::Nested(item) => nested.push(*item),
                },
                _ => {
                    variants.push(self.parse_enum_variant());
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Enum(EnumDecl {
            annotations,
            visibility,
            path,
            type_params,
            variants,
            functions,
            nested,
            span: self.span_from(start),
        })
    }

    fn parse_enum_variant(&mut self) -> EnumVariant {
        let start = self.current_span();
        let name = self.expect_type_ident();

        let data = if self.eat(&TokenKind::LParen).is_some() {
            let types = self.comma_separated(&TokenKind::RParen, Self::parse_type_expr);
            self.expect(&TokenKind::RParen);
            EnumVariantData::Tuple(types)
        } else if self.eat(&TokenKind::LBrace).is_some() {
            let mut fields = Vec::new();
            self.skip_newlines();
            while !self.at(&TokenKind::RBrace) && !self.at_eof() {
                fields.push(self.parse_struct_field());
                if self.eat(&TokenKind::Comma).is_none() {
                    self.skip_newlines();
                    if !self.at(&TokenKind::RBrace) {
                        self.skip_newlines();
                    }
                } else {
                    self.skip_newlines();
                }
            }
            self.expect(&TokenKind::RBrace);
            EnumVariantData::Struct(fields)
        } else {
            EnumVariantData::Unit
        };

        EnumVariant {
            name,
            data,
            span: self.span_from(start),
        }
    }
}
