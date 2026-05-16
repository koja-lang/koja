//! Top-level item declarations. Each kind owns its own submodule;
//! the few helpers that cross kinds live here.
//!
//! Modules:
//! - `alias` — `alias Pkg.Type [as LocalName]` (packages are PascalCase)
//! - `annotation` — `@name`, `@name "value"` decorators on declarations
//! - `constant` — `const NAME [: T] = expr`
//! - `enum_decl` — `enum Name<...> ... end` with Unit / Tuple / Struct variants
//! - `function` — top-level `fn`, parameter lists, body presence
//! - `impl_block` — `impl Target [for Trait] ... end` and inline `type` aliases
//! - `protocol` — `protocol Name<...> ... end` with required/default methods
//! - `struct_decl` — `struct Name<...> ... end` with fields + inline methods
//!
//! The shared body-parsing helpers ([`Parser::parse_block`],
//! [`Parser::parse_optional_type_params`], [`Parser::parse_type_param`])
//! live in this `mod.rs` because every kind of declaration uses them.

pub(crate) mod alias;
pub(crate) mod annotation;
pub(crate) mod constant;
pub(crate) mod enum_decl;
pub(crate) mod function;
pub(crate) mod impl_block;
pub(crate) mod protocol;
pub(crate) mod struct_decl;

use expo_ast::ast::{Statement, TypeParam};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_optional_type_params(&mut self) -> Vec<TypeParam> {
        if self.eat(&TokenKind::Lt).is_none() {
            return Vec::new();
        }
        let mut params = vec![self.parse_type_param()];
        while self.eat(&TokenKind::Comma).is_some() {
            params.push(self.parse_type_param());
        }
        self.expect_gt();
        params
    }

    fn parse_type_param(&mut self) -> TypeParam {
        let span = self.current_span();
        let name = self.expect_type_ident();
        let mut bounds = Vec::new();
        if self.eat(&TokenKind::Colon).is_some() {
            bounds.push(self.expect_type_ident());
            while self.eat(&TokenKind::Ampersand).is_some() {
                bounds.push(self.expect_type_ident());
            }
        }
        TypeParam { name, bounds, span }
    }

    pub(crate) fn parse_block(&mut self) -> Vec<Statement> {
        self.skip_newlines();
        self.parse_until(
            |p| p.at(&TokenKind::End) || p.at(&TokenKind::Else),
            Self::parse_statement,
        )
    }
}
