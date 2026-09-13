//! `test "description" ... end`, at the top level or inside a struct
//! body. The description is a plain string literal. It is a label,
//! not a value, so it does not interpolate.

use koja_ast::ast::TestDecl;
use koja_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_test_decl(&mut self) -> TestDecl {
        let start = self.current_span();
        self.advance(); // test

        let description = self.parse_test_description();

        self.skip_newlines();
        let body = if self.at(&TokenKind::End) {
            self.advance();
            Vec::new()
        } else {
            let statements = self.parse_block();
            self.expect(&TokenKind::End);
            statements
        };

        TestDecl {
            body,
            description,
            span: self.span_from(start),
        }
    }

    /// The string after `test`. A missing or interpolated description
    /// is an error, and the parser recovers with the text it did see.
    fn parse_test_description(&mut self) -> String {
        let span = self.current_span();
        if self.eat(&TokenKind::StringStart).is_none() {
            self.error_with_hint(
                format!(
                    "expected a string description after `test`, found {}",
                    self.peek()
                ),
                "write `test \"what this test checks\"`".into(),
                span,
            );
            return String::new();
        }

        let mut text = String::new();
        loop {
            match self.peek().clone() {
                TokenKind::StringFragment(fragment) => {
                    text.push_str(&fragment);
                    self.advance();
                }
                TokenKind::InterpolStart => {
                    let span = self.current_span();
                    self.error(
                        "a test description is a plain string and cannot interpolate".to_string(),
                        span,
                    );
                    // Skip to the matching `}` so the body still parses.
                    while !self.at(&TokenKind::InterpolEnd) && !self.at_eof() {
                        self.advance();
                    }
                    self.eat(&TokenKind::InterpolEnd);
                }
                TokenKind::StringEnd => {
                    self.advance();
                    break;
                }
                _ => break,
            }
        }
        text
    }
}
