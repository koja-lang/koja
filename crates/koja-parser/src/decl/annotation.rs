//! `@name [value]` decorators on declarations.
//!
//! A declaration may be preceded by zero or more annotations. Each
//! is `@name` optionally followed by a single value: `false`, a
//! quoted string, or a triple-quoted string. Other shapes (numbers,
//! identifiers, structured values) are not currently part of the
//! surface.

use koja_ast::ast::{Annotation, AnnotationValue};
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_annotations(&mut self) -> Vec<Annotation> {
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            annotations.push(self.parse_annotation());
            self.skip_newlines();
        }
        annotations
    }

    pub(crate) fn parse_annotation(&mut self) -> Annotation {
        let start = self.current_span();
        self.advance(); // @
        let name = self.expect_ident();
        let value = self.parse_annotation_value();
        Annotation {
            name,
            value,
            span: self.span_from(start),
        }
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
