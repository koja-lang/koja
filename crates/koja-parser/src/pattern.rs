use koja_ast::ast::*;
use koja_ast::token::TokenKind;

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
                // Legacy lowercase-package qualifier: `pkg.Type.Variant`.
                // Canonical packages are now PascalCase (`Pkg.Type.Variant`)
                // and flow through the `TokenKind::TypeIdent` arm below.
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
                // Typed binding: `name: Type` -- matches a union member by type
                if self.at(&TokenKind::Colon) && matches!(self.peek_nth(1), TokenKind::TypeIdent(_))
                {
                    self.advance(); // :
                    let type_expr = self.parse_type_expr();
                    return Pattern::TypedBinding {
                        local_id: None,
                        name,
                        resolved_type: None,
                        type_expr,
                        span: self.span_from(start),
                    };
                }
                Pattern::Binding {
                    local_id: None,
                    name,
                    span: self.span_from(start),
                }
            }
            TokenKind::IntLit(_) | TokenKind::FloatLit(_) => {
                let start = self.current_span();
                let lit = self.parse_literal_value();
                Pattern::Literal {
                    literal_coercion: None,
                    span: self.span_from(start),
                    value: lit,
                }
            }
            TokenKind::True => {
                let start = self.current_span();
                self.advance();
                Pattern::Literal {
                    literal_coercion: None,
                    span: self.span_from(start),
                    value: Literal::Bool(true),
                }
            }
            TokenKind::False => {
                let start = self.current_span();
                self.advance();
                Pattern::Literal {
                    literal_coercion: None,
                    span: self.span_from(start),
                    value: Literal::Bool(false),
                }
            }
            TokenKind::StringStart => self.parse_string_pattern(false),
            TokenKind::MultilineStringStart => self.parse_string_pattern(true),
            TokenKind::TypeIdent(_) => self.parse_type_pattern(),
            TokenKind::LParen => self.parse_tuple_pattern(),
            TokenKind::LBracket => self.parse_list_pattern(),
            TokenKind::LtLt => self.parse_binary_pattern(),
            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                match self.peek().clone() {
                    TokenKind::IntLit(n) => {
                        self.advance();
                        Pattern::Literal {
                            literal_coercion: None,
                            span: self.span_from(start),
                            value: Literal::Int(format!("-{n}")),
                        }
                    }
                    TokenKind::FloatLit(n) => {
                        self.advance();
                        Pattern::Literal {
                            literal_coercion: None,
                            span: self.span_from(start),
                            value: Literal::Float(format!("-{n}")),
                        }
                    }
                    _ => {
                        let span = self.current_span();
                        self.error(
                            format!(
                                "expected number after `-` in pattern, found {}",
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
                self.error(format!("expected pattern, found {}", self.peek()), span);
                self.advance();
                Pattern::Wildcard { span }
            }
        }
    }

    /// Parse a quoted or triple-quoted string literal in pattern
    /// position. Reuses [`Self::parse_string_expr`] so multiline
    /// dedenting matches expression-position literals exactly.
    /// Interpolation has no meaning in a pattern and is diagnosed.
    fn parse_string_pattern(&mut self, multiline: bool) -> Pattern {
        let start = self.current_span();
        let expr = self.parse_string_expr(multiline);
        let ExprKind::String { parts, .. } = expr.kind else {
            unreachable!("parse_string_expr always yields ExprKind::String");
        };
        let mut text = String::new();
        for part in parts {
            match part {
                StringPart::Literal { value, .. } => text.push_str(&value),
                StringPart::Interpolation { span, .. } => {
                    self.error(
                        "string patterns cannot contain `#{...}` interpolation".to_string(),
                        span,
                    );
                }
            }
        }
        Pattern::Literal {
            literal_coercion: None,
            span: self.span_from(start),
            value: Literal::String(text),
        }
    }

    pub(crate) fn parse_or_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        let first = self.parse_pattern();
        if !self.at(&TokenKind::Pipe) {
            return first;
        }
        let mut patterns = vec![first];
        while self.eat(&TokenKind::Pipe).is_some() {
            patterns.push(self.parse_pattern());
        }
        Pattern::Or {
            span: self.span_from(start),
            patterns,
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
            let elements = self.comma_separated(&TokenKind::RParen, Self::parse_pattern);
            self.expect(&TokenKind::RParen);
            return Pattern::Constructor {
                name: first,
                elements,
                span: self.span_from(start),
            };
        }

        // Plain struct destructuring: Point{x: 5, y: 2}, Point{x, y}, Point{}
        if self.eat(&TokenKind::LBrace).is_some() {
            let fields = self.parse_struct_field_block();
            return Pattern::Struct {
                type_path: vec![first],
                fields,
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
        start: koja_ast::span::Span,
    ) -> Pattern {
        if self.eat(&TokenKind::LParen).is_some() {
            let elements = self.comma_separated(&TokenKind::RParen, Self::parse_pattern);
            self.expect(&TokenKind::RParen);
            Pattern::EnumTuple {
                type_path,
                variant,
                elements,
                span: self.span_from(start),
            }
        } else if self.eat(&TokenKind::LBrace).is_some() {
            let fields = self.parse_struct_field_block();
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

        self.skip_newlines();
        if self.eat(&TokenKind::RParen).is_some() {
            return Pattern::Literal {
                literal_coercion: None,
                span: self.span_from(start),
                value: Literal::Unit,
            };
        }

        let first = self.parse_pattern();
        if self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            while !self.at(&TokenKind::RParen) && !self.at_eof() {
                self.parse_pattern();
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
                self.skip_newlines();
            }
            self.skip_newlines();
            self.expect(&TokenKind::RParen);
            let span = self.span_from(start);
            self.error(
                "tuples are not supported, use a struct instead".to_string(),
                span,
            );
            Pattern::Wildcard { span }
        } else {
            self.skip_newlines();
            self.expect(&TokenKind::RParen);
            first
        }
    }

    fn parse_list_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        self.advance(); // [
        let elements = self.comma_separated(&TokenKind::RBracket, Self::parse_pattern);
        self.expect(&TokenKind::RBracket);
        Pattern::List {
            elements,
            span: self.span_from(start),
        }
    }

    fn parse_binary_pattern(&mut self) -> Pattern {
        let start = self.current_span();
        self.advance(); // <<
        let segments = self.parse_binary_segments();
        Pattern::Binary {
            segments,
            span: self.span_from(start),
        }
    }

    /// Parses a brace-delimited list of field patterns (the `{...}` shared
    /// by `Pattern::EnumStruct` and `Pattern::Struct`). Assumes the opening
    /// `{` has already been consumed and consumes through the matching `}`.
    /// Trailing commas and an empty field list are both legal.
    fn parse_struct_field_block(&mut self) -> Vec<FieldPattern> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            fields.push(self.parse_field_pattern());
            self.eat(&TokenKind::Comma);
            self.skip_newlines();
        }
        self.expect(&TokenKind::RBrace);
        fields
    }

    fn parse_field_pattern(&mut self) -> FieldPattern {
        let start = self.current_span();
        let name = self.expect_ident();
        if self.eat(&TokenKind::Colon).is_none() {
            let span = self.current_span();
            self.error_with_hint(
                format!("expected `:` after field name `{name}` in struct pattern"),
                format!("write `{name}: {name}` to bind under the field name, or omit the field entirely"),
                span,
            );
        }
        let pattern = self.parse_pattern();
        FieldPattern {
            name,
            pattern,
            span: self.span_from(start),
        }
    }
}
