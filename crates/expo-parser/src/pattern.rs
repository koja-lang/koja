use expo_ast::ast::*;
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        match self.peek().clone() {
            TokenKind::Ident(name) if name == "_" => {
                let start = self.current_span();
                self.advance();
                Pattern::Wildcard {
                    span: self.span_from(start),
                }
            }
            TokenKind::Ident(name) => {
                let start = self.current_span();
                self.advance();
                // Check for module-qualified enum pattern: module.Type.Variant
                if self.at(&TokenKind::Dot) && matches!(self.peek_nth(1), TokenKind::TypeIdent(_)) {
                    let mut type_path = vec![name];
                    self.advance(); // .
                    let next = self.expect_type_ident();
                    type_path.push(next);
                    while self.at(&TokenKind::Dot)
                        && matches!(self.peek_nth(1), TokenKind::TypeIdent(_))
                    {
                        self.advance(); // .
                        type_path.push(self.expect_type_ident());
                    }
                    let variant = type_path.pop().unwrap();
                    return self.finish_enum_pattern(type_path, variant, start);
                }
                Pattern::Binding {
                    name,
                    span: self.span_from(start),
                }
            }
            TokenKind::IntLit(_) | TokenKind::FloatLit(_) => {
                let start = self.current_span();
                let lit = self.parse_literal_value();
                Pattern::Literal {
                    value: lit,
                    span: self.span_from(start),
                }
            }
            TokenKind::True => {
                let start = self.current_span();
                self.advance();
                Pattern::Literal {
                    value: Literal::Bool(true),
                    span: self.span_from(start),
                }
            }
            TokenKind::False => {
                let start = self.current_span();
                self.advance();
                Pattern::Literal {
                    value: Literal::Bool(false),
                    span: self.span_from(start),
                }
            }
            TokenKind::None_ => {
                let start = self.current_span();
                self.advance();
                Pattern::Literal {
                    value: Literal::None,
                    span: self.span_from(start),
                }
            }
            TokenKind::StringStart => {
                let start = self.current_span();
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
                Pattern::Literal {
                    value: Literal::Int(text), // Reuse Int for string pattern placeholder
                    span: self.span_from(start),
                }
            }
            TokenKind::TypeIdent(_) => self.parse_type_pattern(),
            TokenKind::LParen => self.parse_tuple_pattern(),
            TokenKind::LBracket => self.parse_list_pattern(),
            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                match self.peek().clone() {
                    TokenKind::IntLit(n) => {
                        self.advance();
                        Pattern::Literal {
                            value: Literal::Int(format!("-{n}")),
                            span: self.span_from(start),
                        }
                    }
                    TokenKind::FloatLit(n) => {
                        self.advance();
                        Pattern::Literal {
                            value: Literal::Float(format!("-{n}")),
                            span: self.span_from(start),
                        }
                    }
                    _ => {
                        let span = self.current_span();
                        self.error(
                            format!(
                                "expected number after '-' in pattern, found {:?}",
                                self.peek()
                            ),
                            span,
                        );
                        Pattern::Wildcard { span }
                    }
                }
            }
            _ => {
                let span = self.current_span();
                self.error(format!("expected pattern, found {:?}", self.peek()), span);
                self.advance();
                Pattern::Wildcard { span }
            }
        }
    }

    fn parse_type_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        let first = self.expect_type_ident();

        // Collect dotted segments: Type.Sub.Variant
        if self.at(&TokenKind::Dot) && matches!(self.peek_nth(1), TokenKind::TypeIdent(_)) {
            let mut segments = vec![first];
            while self.at(&TokenKind::Dot) && matches!(self.peek_nth(1), TokenKind::TypeIdent(_)) {
                self.advance(); // .
                segments.push(self.expect_type_ident());
            }
            let variant = segments.pop().unwrap();
            return self.finish_enum_pattern(segments, variant, start);
        }

        // Constructor shorthand: Some(x), Ok(x), Err(x)
        if self.eat(&TokenKind::LParen).is_some() {
            let mut elements = Vec::new();
            if !self.at(&TokenKind::RParen) {
                elements.push(self.parse_pattern());
                while self.eat(&TokenKind::Comma).is_some() {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_pattern());
                }
            }
            self.expect(&TokenKind::RParen);
            return Pattern::Constructor {
                name: first,
                elements,
                span: self.span_from(start),
            };
        }

        Pattern::Constructor {
            name: first,
            elements: vec![],
            span: self.span_from(start),
        }
    }

    fn finish_enum_pattern(
        &mut self,
        type_path: Vec<String>,
        variant: String,
        start: expo_ast::span::Span,
    ) -> Pattern {
        if self.eat(&TokenKind::LParen).is_some() {
            let mut elements = Vec::new();
            if !self.at(&TokenKind::RParen) {
                elements.push(self.parse_pattern());
                while self.eat(&TokenKind::Comma).is_some() {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_pattern());
                }
            }
            self.expect(&TokenKind::RParen);
            Pattern::EnumTuple {
                type_path,
                variant,
                elements,
                span: self.span_from(start),
            }
        } else if self.eat(&TokenKind::LBrace).is_some() {
            let mut fields = Vec::new();
            self.skip_newlines();
            while !self.at(&TokenKind::RBrace) && !self.at_eof() {
                fields.push(self.parse_field_pattern());
                self.eat(&TokenKind::Comma);
                self.skip_newlines();
            }
            self.expect(&TokenKind::RBrace);
            Pattern::EnumStruct {
                type_path,
                variant,
                fields,
                span: self.span_from(start),
            }
        } else {
            Pattern::EnumUnit {
                type_path,
                variant,
                span: self.span_from(start),
            }
        }
    }

    fn parse_tuple_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        self.advance(); // (

        if self.eat(&TokenKind::RParen).is_some() {
            return Pattern::Literal {
                value: Literal::Unit,
                span: self.span_from(start),
            };
        }

        let first = self.parse_pattern();
        if self.eat(&TokenKind::Comma).is_some() {
            let mut elements = vec![first];
            if !self.at(&TokenKind::RParen) {
                elements.push(self.parse_pattern());
                while self.eat(&TokenKind::Comma).is_some() {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_pattern());
                }
            }
            self.expect(&TokenKind::RParen);
            let span = self.span_from(start);
            self.error(
                "tuples are not supported, use a struct instead".to_string(),
                span,
            );
            Pattern::Tuple { elements, span }
        } else {
            self.expect(&TokenKind::RParen);
            first // grouping in patterns
        }
    }

    fn parse_list_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        self.advance(); // [

        let mut elements = Vec::new();
        if !self.at(&TokenKind::RBracket) {
            elements.push(self.parse_pattern());
            while self.eat(&TokenKind::Comma).is_some() {
                if self.at(&TokenKind::RBracket) {
                    break;
                }
                elements.push(self.parse_pattern());
            }
        }
        self.expect(&TokenKind::RBracket);

        Pattern::List {
            elements,
            span: self.span_from(start),
        }
    }

    fn parse_field_pattern(&mut self) -> FieldPattern {
        let start = self.current_span();
        let name = self.expect_ident();
        let pattern = if self.eat(&TokenKind::Colon).is_some() {
            Some(self.parse_pattern())
        } else {
            None
        };
        FieldPattern {
            name,
            pattern,
            span: self.span_from(start),
        }
    }
}
