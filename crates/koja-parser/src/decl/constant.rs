//! `const NAME [: T] = expr` at the module level, or inside a
//! `struct`, `enum`, or `builtin` body. A module-level constant can
//! also take a qualified name (`const Duration.ZERO = ...`) to nest
//! under a type declared elsewhere.
//!
//! Constant names may be either `Ident` (lowercase) or `TypeIdent`
//! (PascalCase). Both shapes are accepted at the syntax layer. The
//! owner segments before the leaf must be `TypeIdent`s. Anything
//! else lands a guiding diagnostic and proceeds with an error
//! sentinel so later phases can still walk the rest of the file.

use koja_ast::ast::{Annotation, Constant, Item, Name, Visibility};
use koja_ast::token::TokenKind;

use crate::parser::{ERROR_IDENT, Parser};

impl Parser {
    pub(crate) fn parse_constant_item(
        &mut self,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> Item {
        let start = self.current_span();
        self.expect(&TokenKind::Const);
        let path = self.parse_constant_path();
        let type_annotation = if self.peek() == &TokenKind::Colon {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        self.expect(&TokenKind::Eq);
        let value = self.parse_expr();
        Item::Constant(Constant {
            annotations,
            visibility,
            path,
            type_annotation,
            value,
            span: self.span_from(start),
        })
    }

    /// `NAME`, `name`, or `Owner.Nested.NAME`. A `TypeIdent` segment
    /// followed by `.` continues the path. An `Ident` segment always
    /// ends it, since only the leaf may be lowercase.
    fn parse_constant_path(&mut self) -> Vec<Name> {
        let mut segments = Vec::new();
        loop {
            let span = self.current_span();
            match self.peek().clone() {
                TokenKind::TypeIdent(name) => {
                    self.advance();
                    segments.push(Name::new(name, span));
                    if self.at(&TokenKind::Dot)
                        && matches!(
                            self.peek_nth(1),
                            TokenKind::Ident(_) | TokenKind::TypeIdent(_)
                        )
                    {
                        self.advance(); // .
                        continue;
                    }
                }
                TokenKind::Ident(name) => {
                    self.advance();
                    segments.push(Name::new(name, span));
                }
                other => {
                    self.error(format!("expected constant name, found {other}"), span);
                    self.advance();
                    segments.push(Name::new(ERROR_IDENT, span));
                }
            }
            return segments;
        }
    }
}
