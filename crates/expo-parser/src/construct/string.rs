//! String-literal parsing. Handles quoted strings, multiline
//! triple-quoted strings, and `#{...}` interpolation. For multiline
//! literals the closing quote's column is the indent oracle: every
//! literal fragment is dedented by that many spaces, a leading
//! newline (from starting on the line after `"""`) is stripped, and
//! a trailing newline (from the closing quote sitting on its own
//! line) is trimmed.

use expo_ast::ast::{Expr, ExprKind, StringPart};
use expo_ast::token::TokenKind;

use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_string_expr(&mut self, multiline: bool) -> Expr {
        let start = self.current_span();
        self.advance(); // StringStart or MultilineStringStart

        let mut parts = Vec::new();
        let mut closing_column: Option<u32> = None;
        loop {
            match self.peek().clone() {
                TokenKind::StringFragment(text) => {
                    let frag_start = self.current_span();
                    self.advance();
                    parts.push(StringPart::Literal {
                        value: text,
                        span: self.span_from(frag_start),
                    });
                }
                TokenKind::InterpolStart => {
                    let interp_start = self.current_span();
                    self.advance(); // InterpolStart
                    let expr = self.parse_expr();
                    let format = if self.eat(&TokenKind::Colon).is_some() {
                        if let TokenKind::Ident(spec) = self.peek().clone() {
                            self.advance();
                            Some(spec)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    self.expect(&TokenKind::InterpolEnd);
                    parts.push(StringPart::Interpolation {
                        expr: Box::new(expr),
                        format,
                        span: self.span_from(interp_start),
                    });
                }
                TokenKind::StringEnd | TokenKind::MultilineStringEnd => {
                    if multiline {
                        closing_column = Some(self.current_span().start.column);
                    }
                    self.advance();
                    break;
                }
                _ => {
                    self.error("unterminated string".to_string(), self.current_span());
                    break;
                }
            }
        }

        if multiline && let Some(col) = closing_column {
            dedent_multiline_parts(&mut parts, col);
        }

        if parts.is_empty() {
            Expr::new(
                ExprKind::String {
                    parts: vec![StringPart::Literal {
                        value: String::new(),
                        span: self.span_from(start),
                    }],
                    multiline,
                },
                self.span_from(start),
            )
        } else {
            Expr::new(ExprKind::String { parts, multiline }, self.span_from(start))
        }
    }
}

fn dedent_multiline_parts(parts: &mut [StringPart], closing_column: u32) {
    if parts.is_empty() {
        return;
    }
    let indent = (closing_column - 1) as usize;

    if let Some(StringPart::Literal { value, .. }) = parts.first_mut()
        && let Some(stripped) = value.strip_prefix('\n')
    {
        *value = stripped.to_string();
    }

    for (i, part) in parts.iter_mut().enumerate() {
        if let StringPart::Literal { value, .. } = part {
            *value = dedent_string(value, indent, i == 0);
        }
    }

    if let Some(StringPart::Literal { value, .. }) = parts.last_mut()
        && value.ends_with('\n')
    {
        value.pop();
    }
}

fn dedent_string(s: &str, indent: usize, dedent_first_line: bool) -> String {
    let mut result = String::with_capacity(s.len());
    let mut at_line_start = dedent_first_line;
    let mut stripped = 0;

    for ch in s.chars() {
        if ch == '\n' {
            result.push('\n');
            at_line_start = true;
            stripped = 0;
        } else if at_line_start && ch == ' ' && stripped < indent {
            stripped += 1;
        } else {
            at_line_start = false;
            result.push(ch);
        }
    }

    result
}
