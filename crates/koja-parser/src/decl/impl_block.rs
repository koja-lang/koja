//! `impl Trait for Target ... end` declares protocol conformance.
//! Bare `impl Type` is rejected with a migration diagnostic and
//! recovered as [`ExtendBlock`] so the rest of the file still parses.

use koja_ast::ast::{ExtendBlock, ImplBlock, ImplMember, Item, TestDecl, Visibility};
use koja_ast::token::TokenKind;

use crate::decl::struct_decl::TypeBodyMember;
use crate::parser::Parser;

/// The parsed body of an `impl` or `extend` block.
#[derive(Default)]
pub(crate) struct ImplBody {
    pub(crate) members: Vec<ImplMember>,
    pub(crate) tests: Vec<TestDecl>,
}

impl Parser {
    pub(crate) fn parse_impl_item(&mut self) -> Item {
        let start = self.current_span();
        let impl_span = self.current_span();
        self.advance();

        let first_type = self.parse_type_expr();
        if self.eat(&TokenKind::For).is_none() {
            self.error_with_hint(
                "bare `impl Type` is not supported. Use `extend Type` for inherent methods"
                    .to_string(),
                "replace `impl` with `extend`. `impl` is reserved for protocol \
                 implementations: `impl Protocol for Type`. If you meant to implement a \
                 protocol, add `for <Protocol>` after the type."
                    .to_string(),
                impl_span,
            );
            let body = self.parse_impl_members();
            self.expect(&TokenKind::End);
            return Item::Extend(ExtendBlock {
                target: first_type,
                members: body.members,
                span: self.span_from(start),
                tests: body.tests,
            });
        }
        let (target, target_bounds) = self.parse_impl_target();
        let body = self.parse_impl_members();
        self.expect(&TokenKind::End);

        Item::Impl(ImplBlock {
            target,
            target_bounds,
            trait_expr: first_type,
            members: body.members,
            span: self.span_from(start),
            tests: body.tests,
        })
    }

    /// Parse the body of an `impl` or `extend` block (methods, inline
    /// `type` aliases, and `test` blocks). Leaves the trailing `end`
    /// for the caller to consume.
    pub(crate) fn parse_impl_members(&mut self) -> ImplBody {
        self.skip_newlines();
        let mut body = ImplBody::default();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn
                | TokenKind::Priv
                | TokenKind::At
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Protocol
                | TokenKind::Const => {
                    let member_span = self.current_span();
                    match self.parse_type_body_member("impl") {
                        TypeBodyMember::Function(func) => {
                            body.members.push(ImplMember::Function(*func));
                        }
                        TypeBodyMember::Nested(_) => {
                            self.error_with_hint(
                                "nested type and constant declarations are not valid in `impl` \
                                 or `extend` blocks"
                                    .to_string(),
                                "declare it inside the owner's body or at the top level with a \
                                 qualified name"
                                    .to_string(),
                                member_span,
                            );
                        }
                    }
                }
                TokenKind::Type => {
                    let alias = self.parse_type_alias(Vec::new(), Visibility::Public);
                    body.members.push(ImplMember::TypeAlias(alias));
                }
                TokenKind::Test => body.tests.push(self.parse_test_decl()),
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function, type alias, or test in block body, found {}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        body
    }
}
