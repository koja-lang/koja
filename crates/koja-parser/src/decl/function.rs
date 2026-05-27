//! Function declarations.
//!
//! A function has annotations, visibility (`pub` by default, `priv`
//! to make file-local), a name, optional `<T>` type parameters, a
//! parameter list (which may be empty), an optional `-> ReturnType`,
//! and a body. Bodies are omitted for `@extern` / `@intrinsic`
//! signatures and, inside struct/enum/impl bodies, when the *next*
//! token starts another function declaration.

use koja_ast::ast::{Annotation, Function, Item, Param, PassMode, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_function_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        Item::Function(self.parse_function_decl(annotations, visibility))
    }

    pub(crate) fn parse_function_decl(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Function {
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
        let bodyless_marker = annotations
            .iter()
            .any(|a| a.name == "extern" || a.name == "intrinsic");
        let body = if bodyless_marker
            || self.at(&TokenKind::Fn)
            || self.at(&TokenKind::At)
            || (self.at(&TokenKind::Priv) && matches!(self.peek_nth(1), TokenKind::Fn))
        {
            None
        } else if self.at(&TokenKind::End) {
            self.advance();
            Some(Vec::new())
        } else {
            let stmts = self.parse_block();
            self.expect(&TokenKind::End);
            Some(stmts)
        };

        Function {
            annotations,
            visibility,
            name,
            type_params,
            params,
            return_type,
            body,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_param_list(&mut self) -> Vec<Param> {
        self.comma_separated(&TokenKind::RParen, Self::parse_param)
    }

    fn parse_param(&mut self) -> Param {
        let start = self.current_span();

        let has_move = self.eat(&TokenKind::Move).is_some();

        if self.eat(&TokenKind::Self_).is_some() {
            return Param::Self_ {
                mode: if has_move {
                    PassMode::Move
                } else {
                    PassMode::Borrow
                },
                local_id: None,
                span: self.span_from(start),
            };
        }

        let mode = if has_move || self.eat(&TokenKind::Move).is_some() {
            PassMode::Move
        } else {
            PassMode::Borrow
        };
        let name = self.expect_ident();
        self.expect(&TokenKind::Colon);
        let type_expr = self.parse_type_expr();
        let default = if self.eat(&TokenKind::Eq).is_some() {
            Some(self.parse_expr())
        } else {
            None
        };

        Param::Regular {
            mode,
            name,
            type_expr,
            default,
            local_id: None,
            span: self.span_from(start),
        }
    }
}
