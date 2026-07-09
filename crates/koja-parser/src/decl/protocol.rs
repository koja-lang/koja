//! `protocol Name<...> ... end`.
//!
//! Protocol bodies hold method signatures (optionally with default
//! bodies) and `@annotation`-prefixed signatures. Anything else in
//! the body is a parse error with a guiding diagnostic. Method
//! signatures use the same parameter / return-type grammar as
//! top-level `fn`, but the body is optional (omitted bodies indicate
//! a required method without a default).

use koja_ast::ast::{Annotation, Item, ProtocolDecl, ProtocolMethod, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_protocol_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // protocol

        let name = self.expect_type_ident();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn => {
                    methods.push(self.parse_protocol_method(Vec::new()));
                }
                TokenKind::At => {
                    let anns = self.parse_annotations();
                    self.skip_newlines();
                    if self.at(&TokenKind::Fn) {
                        methods.push(self.parse_protocol_method(anns));
                    } else {
                        let span = self.current_span();
                        self.error(
                            "annotation in protocol must be followed by a function signature"
                                .to_string(),
                            span,
                        );
                    }
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function signature in protocol, found {}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Protocol(ProtocolDecl {
            annotations,
            visibility,
            name,
            type_params,
            methods,
            span: self.span_from(start),
        })
    }

    fn parse_protocol_method(&mut self, annotations: Vec<Annotation>) -> ProtocolMethod {
        let start = self.current_span();
        self.advance(); // fn

        let name = self.expect_ident();
        let type_params = self.parse_optional_type_params();

        let params = if self.eat(&TokenKind::LParen).is_some() {
            let params = self.parse_param_list();
            self.expect(&TokenKind::RParen);
            params
        } else {
            Vec::new()
        };

        self.skip_newlines();
        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.skip_newlines();
        let body =
            if !self.at(&TokenKind::End) && !self.at(&TokenKind::Fn) && !self.at(&TokenKind::At) {
                let stmts = self.parse_block();
                self.expect(&TokenKind::End);
                Some(stmts)
            } else {
                None
            };

        ProtocolMethod {
            annotations,
            name,
            type_params,
            params,
            return_type,
            body,
            span: self.span_from(start),
        }
    }
}
