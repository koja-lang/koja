//! Type construction expressions: struct literal `Type { field: ... }`,
//! enum variant construction (`Variant`, `Variant(...)`, `Variant{...}`),
//! and package-qualified forms like `Pkg.Type` and `Pkg.Type.Variant`
//! (packages are PascalCase).
//!
//! The shape of the input drives the result:
//! - `Type` (bare TypeIdent) on its own becomes an `Ident` (a type
//!   reference, resolved later).
//! - `Type {...}` builds a `StructConstruction`.
//! - `Type(...)` reuses the `Ident` form as a callable.
//! - `path.Type.Variant`, possibly followed by `(...)` or `{...}`,
//!   becomes an `EnumConstruction` whose `type_path` is everything
//!   before the variant name.
//!
//! `extract_type_path` is the dual helper used by the postfix
//! `.Variant` path in the Pratt loop to flatten an `Ident` /
//! `FieldAccess` / unit-`EnumConstruction` prefix into the type-path
//! prefix of an enum construction.

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, FieldInit};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use crate::parser::{ERROR_IDENT, Parser};

impl Parser {
    pub(crate) fn parse_type_construction(&mut self) -> Expr {
        let start = self.current_span();
        let first = self.expect_type_ident();
        let mut path = vec![first];

        while self.at(&TokenKind::Dot) {
            if matches!(self.peek_nth(1), TokenKind::TypeIdent(_)) {
                self.advance(); // .
                let seg = self.expect_type_ident();

                if self.at(&TokenKind::LBrace)
                    || self.at(&TokenKind::LParen)
                    || self.at(&TokenKind::Dot)
                {
                    if self.at(&TokenKind::Dot)
                        && matches!(self.peek_nth(1), TokenKind::TypeIdent(_))
                    {
                        path.push(seg);
                        continue;
                    }
                    return self.parse_enum_construction_tail(path, seg, start);
                } else {
                    return Expr::new(
                        ExprKind::EnumConstruction {
                            type_path: path,
                            variant: seg,
                            data: EnumConstructionData::Unit,
                        },
                        self.span_from(start),
                    );
                }
            } else {
                break;
            }
        }

        if self.at(&TokenKind::LBrace) {
            self.advance(); // {
            let fields = self.parse_field_init_block();
            Expr::new(
                ExprKind::StructConstruction {
                    type_path: path,
                    fields,
                },
                self.span_from(start),
            )
        } else if self.at(&TokenKind::LParen) {
            self.advance(); // (
            let args = self.parse_arg_list();
            self.expect(&TokenKind::RParen);
            let callee = Expr::new(
                ExprKind::Ident {
                    name: path.join("."),
                    resolution: Resolution::Unresolved,
                },
                self.span_from(start),
            );
            Expr::new(
                ExprKind::Call {
                    callee: Box::new(callee),
                    args,
                    type_args: Vec::new(),
                },
                self.span_from(start),
            )
        } else {
            Expr::new(
                ExprKind::Ident {
                    name: path.join("."),
                    resolution: Resolution::Unresolved,
                },
                self.span_from(start),
            )
        }
    }

    pub(crate) fn parse_enum_construction_tail(
        &mut self,
        type_path: Vec<String>,
        variant: String,
        start: Span,
    ) -> Expr {
        if self.eat(&TokenKind::LParen).is_some() {
            let args = self.comma_separated(&TokenKind::RParen, Self::parse_expr);
            self.expect(&TokenKind::RParen);
            let data = if args.is_empty() {
                EnumConstructionData::Unit
            } else {
                EnumConstructionData::Tuple(args)
            };
            Expr::new(
                ExprKind::EnumConstruction {
                    type_path,
                    variant,
                    data,
                },
                self.span_from(start),
            )
        } else if self.eat(&TokenKind::LBrace).is_some() {
            let fields = self.parse_field_init_block();
            Expr::new(
                ExprKind::EnumConstruction {
                    type_path,
                    variant,
                    data: EnumConstructionData::Struct(fields),
                },
                self.span_from(start),
            )
        } else {
            Expr::new(
                ExprKind::EnumConstruction {
                    type_path,
                    variant,
                    data: EnumConstructionData::Unit,
                },
                self.span_from(start),
            )
        }
    }

    /// Parse a `{ name: expr, ... }` block of named field
    /// initializers, shared between struct construction and the
    /// struct-shape enum variant. The opening `{` must already be
    /// consumed. This consumes through the matching `}`. A trailing
    /// comma is tolerated and an empty body produces an empty
    /// `Vec`.
    fn parse_field_init_block(&mut self) -> Vec<FieldInit> {
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let field_start = self.current_span();
            let name = self.expect_ident();
            self.expect(&TokenKind::Colon);
            let value = self.parse_expr();
            fields.push(FieldInit {
                name,
                value,
                span: self.span_from(field_start),
            });
            self.eat(&TokenKind::Comma);
            self.skip_newlines();
        }
        self.expect(&TokenKind::RBrace);
        fields
    }

    /// Flatten an expression that names a type into a path of
    /// segments. Used by the postfix `.Variant` rule in the Pratt
    /// loop: `pkg.Type.Variant` parses the `pkg.Type` prefix as a
    /// `FieldAccess` (or a unit `EnumConstruction` for deeper paths),
    /// which then needs to be unrolled into `["pkg", "Type"]` so the
    /// outer `EnumConstruction` carries the right `type_path` and
    /// `variant`.
    pub(crate) fn extract_type_path(&self, expr: &Expr) -> Vec<String> {
        match &expr.kind {
            ExprKind::Ident { name, .. } => vec![name.clone()],
            ExprKind::FieldAccess {
                receiver, field, ..
            } => {
                let mut path = self.extract_type_path(receiver);
                path.push(field.clone());
                path
            }
            ExprKind::EnumConstruction {
                type_path,
                variant,
                data: EnumConstructionData::Unit,
            } => {
                let mut path = type_path.clone();
                path.push(variant.clone());
                path
            }
            _ => vec![ERROR_IDENT.to_string()],
        }
    }
}
