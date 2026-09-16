//! Type aliases. Two surface forms:
//!
//! - `type Name = TypeExpr`: a local rename for a type expression.
//!   Lives both at the top level (as an `Item::TypeAlias`) and inside
//!   `impl` bodies (as an `ImplMember::TypeAlias`).
//! - `alias Pkg.Name [as LocalName]`: bind a foreign-package type,
//!   function, or constant in the current file, optionally renaming
//!   it. Package names are PascalCase (e.g. `Net`, `HTTP`, `JSON`).
//!   The path ends with a `TypeIdent` for a type or constant, or one
//!   `Ident` for a function. Typecheck checks that the local name
//!   has the same case as the target.

use koja_ast::ast::{AliasDecl, Annotation, Item, Name, TypeAlias, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_type_alias(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> TypeAlias {
        let start = self.current_span();
        self.advance(); // type
        let name = self.expect_type_name();
        self.expect(&TokenKind::Eq);
        let type_expr = self.parse_type_expr();
        TypeAlias {
            annotations,
            visibility,
            name,
            type_expr,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_type_alias_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let alias = self.parse_type_alias(annotations, visibility);
        Item::TypeAlias(alias)
    }

    pub(crate) fn parse_alias_item(&mut self) -> Item {
        let start = self.current_span();
        self.advance(); // alias

        let path = self.parse_alias_path();
        let local_name = if self.eat(&TokenKind::Ident("as".to_string())).is_some() {
            self.expect_any_name()
        } else {
            path.last()
                .cloned()
                .unwrap_or_else(|| Name::new(String::new(), self.current_span()))
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
    /// parser bailing out. Phase 2 consumes one or more `TypeIdent`
    /// segments separated by `.`, and one trailing `Ident` segment
    /// ends the path as a function name. Anything else lands a
    /// diagnostic and short-circuits.
    fn parse_alias_path(&mut self) -> Vec<Name> {
        let mut path = Vec::new();

        while matches!(self.peek(), TokenKind::Ident(_)) {
            path.push(self.expect_name());
            if self.eat(&TokenKind::Dot).is_none() {
                self.error(
                    "alias path must be `Package.Name`".to_string(),
                    self.current_span(),
                );
                return path;
            }
        }

        if !matches!(self.peek(), TokenKind::TypeIdent(_)) {
            self.error(
                format!("expected package path in alias, found {}", self.peek()),
                self.current_span(),
            );
            return path;
        }
        path.push(self.expect_type_name());

        while self.eat(&TokenKind::Dot).is_some() {
            match self.peek().clone() {
                TokenKind::TypeIdent(_) => path.push(self.expect_type_name()),
                TokenKind::Ident(_) => {
                    path.push(self.expect_name());
                    return path;
                }
                _ => {
                    self.error(
                        "alias path must be `Package.Name`".to_string(),
                        self.current_span(),
                    );
                    return path;
                }
            }
        }

        path
    }

    /// A local name after `as`, in either case. Typecheck matches the
    /// case against the alias target.
    fn expect_any_name(&mut self) -> Name {
        let span = self.current_span();
        match self.peek().clone() {
            TokenKind::Ident(name) | TokenKind::TypeIdent(name) => {
                self.advance();
                Name::new(name, span)
            }
            _ => {
                self.error(
                    format!("expected a name after `as`, found {}", self.peek()),
                    span,
                );
                self.advance();
                Name::new(String::new(), span)
            }
        }
    }
}
