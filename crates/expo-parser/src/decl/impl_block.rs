//! `impl Trait for Target ... end` blocks declare that `Target`
//! implements `Trait`. The `for` clause is required: bare
//! `impl Type` (inherent methods) is rejected with a migration
//! diagnostic pointing the user to `extend Type`. The parser
//! recovers by emitting an [`ExtendBlock`] so the rest of the file
//! still type-checks meaningfully, but the original `impl`
//! diagnostic remains the user's signal to migrate.

use expo_ast::ast::{ExtendBlock, ImplBlock, ImplMember, Item};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_impl_item(&mut self) -> Item {
        let start = self.current_span();
        let impl_span = self.current_span();
        self.advance();

        let first_type = self.parse_type_expr();
        if self.eat(&TokenKind::For).is_none() {
            self.error_with_hint(
                "bare `impl Type` is not supported; use `extend Type` for inherent methods"
                    .to_string(),
                "replace `impl` with `extend`. `impl` is reserved for protocol \
                 implementations: `impl Protocol for Type`. If you meant to implement a \
                 protocol, add `for <Protocol>` after the type."
                    .to_string(),
                impl_span,
            );
            let members = self.parse_impl_members();
            self.expect(&TokenKind::End);
            return Item::Extend(ExtendBlock {
                target: first_type,
                members,
                span: self.span_from(start),
            });
        }
        let target = self.parse_type_expr();
        let members = self.parse_impl_members();
        self.expect(&TokenKind::End);

        Item::Impl(ImplBlock {
            target,
            trait_expr: first_type,
            members,
            span: self.span_from(start),
        })
    }

    /// Parses the body of an `impl` or `extend` block: zero or more
    /// `fn`/`priv fn` methods and inline `type` aliases, ending at
    /// `end`. The caller is responsible for consuming the trailing
    /// `end` token; this helper leaves it for the caller's
    /// `expect(&TokenKind::End)`.
    pub(crate) fn parse_impl_members(&mut self) -> Vec<ImplMember> {
        self.skip_newlines();
        let mut members = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            self.skip_newlines();
            if self.at(&TokenKind::End) {
                break;
            }
            match self.peek().clone() {
                TokenKind::Fn | TokenKind::Priv | TokenKind::At => {
                    let func = self.parse_type_body_function("impl");
                    members.push(ImplMember::Function(func));
                }
                TokenKind::Type => {
                    let alias = self.parse_type_alias(Vec::new());
                    members.push(ImplMember::TypeAlias(alias));
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        format!(
                            "expected function or type alias in block body, found {:?}",
                            self.peek()
                        ),
                        span,
                    );
                    self.advance();
                }
            }
            self.skip_newlines();
        }
        members
    }
}
