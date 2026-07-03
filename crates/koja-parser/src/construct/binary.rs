//! Binary literal expressions: `<<segment, segment, ...>>`.
//!
//! Each segment is `value` optionally followed by `:: size` (in
//! `bit` by default, or `byte`) plus modifier flags (`signed` /
//! `unsigned`, `big` / `little`). A `: TypeExpr` form replaces the
//! `:: size` form for type-driven encoding.
//!
//! Patterns use the same segment grammar via
//! [`Parser::parse_binary_segment`].

use koja_ast::ast::{
    BinaryEndianness, BinarySegment, BinarySignedness, BinaryUnit, Expr, ExprKind,
};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_binary_literal(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // <<
        let segments = self.parse_binary_segments();
        Expr::new(ExprKind::BinaryLiteral { segments }, self.span_from(start))
    }

    /// Parse the `segment, segment, ..., >>` body shared between the
    /// binary literal expression and the binary pattern. Assumes
    /// the opening `<<` has already been consumed. This consumes
    /// through (and including) the closing `>>`.
    pub(crate) fn parse_binary_segments(&mut self) -> Vec<BinarySegment> {
        let segments = self.comma_separated(&TokenKind::GtGt, Self::parse_binary_segment);
        self.expect(&TokenKind::GtGt);
        segments
    }

    pub(crate) fn parse_binary_segment(&mut self) -> BinarySegment {
        let start = self.current_span();
        let value = Box::new(self.parse_expr());

        let mut size = None;
        let mut unit = BinaryUnit::Bit;
        let mut signedness = None;
        let mut endianness = None;
        let mut type_ann = None;

        if self.eat(&TokenKind::ColonColon).is_some() {
            size = Some(Box::new(self.parse_expr()));

            if self.at_contextual_ident("byte") {
                self.advance();
                unit = BinaryUnit::Byte;
            }

            loop {
                if self.at_contextual_ident("signed") {
                    self.advance();
                    signedness = Some(BinarySignedness::Signed);
                } else if self.at_contextual_ident("unsigned") {
                    self.advance();
                    signedness = Some(BinarySignedness::Unsigned);
                } else if self.at_contextual_ident("big") {
                    self.advance();
                    endianness = Some(BinaryEndianness::Big);
                } else if self.at_contextual_ident("little") {
                    self.advance();
                    endianness = Some(BinaryEndianness::Little);
                } else {
                    break;
                }
            }
        } else if self.at(&TokenKind::Colon) && size.is_none() {
            self.advance();
            type_ann = Some(self.parse_type_expr());
        }

        BinarySegment {
            value,
            size,
            unit,
            signedness,
            endianness,
            type_ann,
            span: self.span_from(start),
        }
    }
}
