//! Type aliases. Two surface forms:
//!
//! - `type Name = TypeExpr`: a local rename for a type expression.
//!   Lives both at the top level (as an `Item::TypeAlias`) and inside
//!   `impl` bodies (as an `ImplMember::TypeAlias`).
//! - `alias Pkg.Type [as LocalName]`: import a foreign-package
//!   type into the current scope, optionally renaming it. Package
//!   names are PascalCase (e.g. `Net`, `HTTP`, `JSON`) and the path
//!   must end with a `TypeIdent` segment.

use koja_ast::ast::{AliasDecl, Annotation, Item, TypeAlias};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_type_alias(&mut self, annotations: Vec<Annotation>) -> TypeAlias {
        let start = self.current_span();
        self.advance(); // type
        let name = self.expect_type_ident();
        self.expect(&TokenKind::Eq);
        let type_expr = self.parse_type_expr();
        TypeAlias {
            annotations,
            name,
            type_expr,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_type_alias_item(&mut self, annotations: Vec<Annotation>) -> Item {
        let alias = self.parse_type_alias(annotations);
        Item::TypeAlias(alias)
    }

    pub(crate) fn parse_alias_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // alias

        let path = self.parse_alias_path();
        let local_name = if self.eat(&TokenKind::Ident("as".to_string())).is_some() {
            self.expect_type_ident()
        } else {
            path.last().cloned().unwrap_or_default()
        };

        Item::Alias(AliasDecl {
            path,
            local_name,
            span: self.span_from(start),
        })
    }

    /// Two-phase alias path parser. Phase 1 absorbs any number of
    /// leading `Ident.` qualifiers. These are never canonical, since
    /// packages are PascalCase, but are accepted as a recovery path
    /// so the resolver can later flag the source rather than the
    /// parser bailing out. Phase 2 then consumes one-or-more
    /// `TypeIdent` segments separated by `.`. The path must end on
    /// a `TypeIdent`. Anything else lands a diagnostic and
    /// short-circuits.
    fn parse_alias_path(&mut self) -> Vec<String> {
        let mut path = Vec::new();

        while matches!(self.peek(), TokenKind::Ident(_)) {
            path.push(self.expect_ident());
            if self.eat(&TokenKind::Dot).is_none() {
                self.error(
                    "alias path must end with a type name (PascalCase)".to_string(),
                    self.current_span(),
                );
                return path;
            }
        }

        if !matches!(self.peek(), TokenKind::TypeIdent(_)) {
            self.error(
                format!("expected package path in alias, found {:?}", self.peek()),
                self.current_span(),
            );
            return path;
        }
        path.push(self.expect_type_ident());

        while self.eat(&TokenKind::Dot).is_some() {
            match self.peek().clone() {
                TokenKind::TypeIdent(_) => path.push(self.expect_type_ident()),
                TokenKind::Ident(name) => {
                    self.advance();
                    path.push(name);
                    self.error(
                        "alias path must end with a type name (PascalCase)".to_string(),
                        self.current_span(),
                    );
                    return path;
                }
                _ => {
                    self.error(
                        "alias path must end with a type name (PascalCase)".to_string(),
                        self.current_span(),
                    );
                    return path;
                }
            }
        }

        path
    }
}
