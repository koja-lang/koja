//! `@name [value]` decorators on declarations.
//!
//! A declaration may be preceded by zero or more annotations. Each
//! is `@name` optionally followed by a single value: `false`, a
//! quoted string, or a triple-quoted string. Other shapes (numbers,
//! identifiers, structured values) are not currently part of the
//! surface. `@test` was removed in 0.20 and is reported with its
//! replacement, the `test "..."` block.

use koja_ast::ast::{Annotation, AnnotationValue};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_annotations(&mut self) -> Vec<Annotation> {
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            if let Some(annotation) = self.parse_annotation() {
                annotations.push(annotation);
            }
            self.skip_newlines();
        }
        annotations
    }

    /// Parses one annotation. A removed `@test` is reported and
    /// dropped, so the declaration it sat on parses as plain and
    /// later passes report nothing spurious.
    pub(crate) fn parse_annotation(&mut self) -> Option<Annotation> {
        let start = self.current_span();
        self.advance(); // @
        if self.eat(&TokenKind::Test).is_some() {
            self.parse_annotation_value();
            self.error_with_hint(
                "`@test` was removed in 0.20".to_string(),
                "move the body into a `test \"description\"` block".to_string(),
                self.span_from(start),
            );
            return None;
        }
        let name = self.expect_ident();
        let value = self.parse_annotation_value();
        Some(Annotation {
            name,
            value,
            span: self.span_from(start),
        })
    }

    fn parse_annotation_value(&mut self) -> Option<AnnotationValue> {
        match self.peek() {
            TokenKind::False => {
                self.advance();
                Some(AnnotationValue::False)
            }
            TokenKind::StringStart => {
                self.advance(); // StringStart
                let mut text = String::new();
                loop {
                    match self.peek().clone() {
                        TokenKind::StringFragment(s) => {
                            text.push_str(&s);
                            self.advance();
                        }
                        TokenKind::StringEnd => {
                            self.advance();
                            break;
                        }
                        _ => break,
                    }
                }
                Some(AnnotationValue::String(text))
            }
            TokenKind::MultilineStringStart => {
                self.advance();
                let mut text = String::new();
                loop {
                    match self.peek().clone() {
                        TokenKind::StringFragment(s) => {
                            text.push_str(&s);
                            self.advance();
                        }
                        TokenKind::MultilineStringEnd => {
                            self.advance();
                            break;
                        }
                        _ => break,
                    }
                }
                Some(AnnotationValue::String(text))
            }
            _ => None,
        }
    }
}
