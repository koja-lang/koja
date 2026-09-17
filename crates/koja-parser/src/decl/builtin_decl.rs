//! `builtin Name<...> ... end`.
//!
//! Declares a compiler-owned type. The body admits functions,
//! constants, and `test` blocks, so fields and nested types are
//! parse errors.

use koja_ast::ast::{Annotation, BuiltinDecl, Item, Visibility};
use koja_ast::token::TokenKind;

use super::struct_decl::TypeBodyMember;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_builtin_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let start = self.current_span();
        self.advance(); // builtin

        let path = self.parse_decl_path();
        let type_params = self.parse_optional_type_params();

        self.skip_newlines();
        let mut functions = Vec::new();
        let mut nested = Vec::new();
        let mut tests = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            match self.peek() {
                TokenKind::Test => tests.push(self.parse_test_decl()),
                TokenKind::Fn | TokenKind::Priv | TokenKind::At | TokenKind::Const => {
                    let member_span = self.current_span();
                    match self.parse_type_body_member("builtin") {
                        TypeBodyMember::Function(function) => functions.push(*function),
                        TypeBodyMember::Nested(item) if matches!(*item, Item::Constant(_)) => {
                            nested.push(*item);
                        }
                        TypeBodyMember::Nested(_) => {
                            self.error(
                                "builtin blocks cannot declare nested types".to_string(),
                                member_span,
                            );
                        }
                    }
                }
                other => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected a function or constant declaration in builtin block, \
                             found {other}. The compiler provides the representation of a \
                             builtin type"
                        ),
                        span,
                    );
                    while !self.at(&TokenKind::Newline) && !self.at_eof() {
                        self.advance();
                    }
                }
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Item::Builtin(BuiltinDecl {
            annotations,
            visibility,
            path,
            type_params,
            functions,
            nested,
            span: self.span_from(start),
            tests,
        })
    }
}
