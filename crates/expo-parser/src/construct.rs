use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_ast::token::TokenKind;

use crate::expr::expr_span;
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
                        expr,
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
            Expr::String {
                parts: vec![StringPart::Literal {
                    value: String::new(),
                    span: self.span_from(start),
                }],
                multiline,
                span: self.span_from(start),
            }
        } else {
            Expr::String {
                parts,
                multiline,
                span: self.span_from(start),
            }
        }
    }

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
                    return Expr::EnumConstruction {
                        type_path: path,
                        variant: seg,
                        data: EnumConstructionData::Unit,
                        span: self.span_from(start),
                    };
                }
            } else {
                break;
            }
        }

        if self.at(&TokenKind::LBrace) {
            self.advance(); // {
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
            Expr::StructConstruction {
                type_path: path,
                fields,
                span: self.span_from(start),
            }
        } else if self.at(&TokenKind::LParen) {
            self.advance(); // (
            let args = self.parse_arg_list();
            self.expect(&TokenKind::RParen);
            let callee = Expr::Ident {
                name: path.into_iter().collect::<Vec<_>>().join("."),
                span: self.span_from(start),
            };
            Expr::Call {
                callee: Box::new(callee),
                args,
                span: self.span_from(start),
            }
        } else {
            Expr::Ident {
                name: path.join("."),
                span: self.span_from(start),
            }
        }
    }

    pub(crate) fn parse_enum_construction_tail(
        &mut self,
        type_path: Vec<String>,
        variant: String,
        start: Span,
    ) -> Expr {
        if self.eat(&TokenKind::LParen).is_some() {
            let mut args = Vec::new();
            if !self.at(&TokenKind::RParen) {
                args.push(self.parse_expr());
                while self.eat(&TokenKind::Comma).is_some() {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    args.push(self.parse_expr());
                }
            }
            self.expect(&TokenKind::RParen);
            let data = if args.is_empty() {
                EnumConstructionData::Unit
            } else {
                EnumConstructionData::Tuple(args)
            };
            Expr::EnumConstruction {
                type_path,
                variant,
                data,
                span: self.span_from(start),
            }
        } else if self.eat(&TokenKind::LBrace).is_some() {
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
            Expr::EnumConstruction {
                type_path,
                variant,
                data: EnumConstructionData::Struct(fields),
                span: self.span_from(start),
            }
        } else {
            Expr::EnumConstruction {
                type_path,
                variant,
                data: EnumConstructionData::Unit,
                span: self.span_from(start),
            }
        }
    }

    pub(crate) fn parse_paren_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // (

        if self.eat(&TokenKind::RParen).is_some() {
            return Expr::Literal {
                value: Literal::Unit,
                span: self.span_from(start),
            };
        }

        let first = self.parse_expr();

        if self.eat(&TokenKind::Comma).is_some() {
            let mut elements = vec![first];
            if !self.at(&TokenKind::RParen) {
                elements.push(self.parse_expr());
                while self.eat(&TokenKind::Comma).is_some() {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    elements.push(self.parse_expr());
                }
            }
            self.expect(&TokenKind::RParen);
            let span = self.span_from(start);
            self.error(
                "tuples are not supported, use a struct instead".to_string(),
                span,
            );
            Expr::Tuple { elements, span }
        } else {
            self.expect(&TokenKind::RParen);
            Expr::Group {
                expr: Box::new(first),
                span: self.span_from(start),
            }
        }
    }

    pub(crate) fn parse_list_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // [

        self.skip_newlines();

        // Empty map literal: [:]
        if self.at(&TokenKind::Colon) && self.peek_nth(1) == &TokenKind::RBracket {
            self.advance(); // :
            self.advance(); // ]
            return Expr::Map {
                entries: Vec::new(),
                span: self.span_from(start),
            };
        }

        if self.at(&TokenKind::RBracket) {
            self.advance(); // ]
            return Expr::List {
                elements: Vec::new(),
                span: self.span_from(start),
            };
        }

        let first = self.parse_expr();

        // If followed by `:`, this is a map literal
        if self.eat(&TokenKind::Colon).is_some() {
            self.skip_newlines();
            let first_val = self.parse_expr();
            let mut entries = vec![(first, first_val)];
            while self.eat(&TokenKind::Comma).is_some() {
                self.skip_newlines();
                if self.at(&TokenKind::RBracket) {
                    break;
                }
                let key = self.parse_expr();
                self.expect(&TokenKind::Colon);
                self.skip_newlines();
                let val = self.parse_expr();
                entries.push((key, val));
            }
            self.skip_newlines();
            self.expect(&TokenKind::RBracket);
            return Expr::Map {
                entries,
                span: self.span_from(start),
            };
        }

        // Otherwise it's a list literal
        let mut elements = vec![first];
        while self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            if self.at(&TokenKind::RBracket) {
                break;
            }
            elements.push(self.parse_expr());
        }
        self.skip_newlines();
        self.expect(&TokenKind::RBracket);

        Expr::List {
            elements,
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_await_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // await
        let expr = self.parse_expr();

        Expr::Await {
            expr: Box::new(expr),
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_spawn_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // spawn
        let expr = self.parse_expr();

        Expr::Spawn {
            expr: Box::new(expr),
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_closure_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // fn

        self.expect(&TokenKind::LParen);
        let params = if self.at(&TokenKind::RParen) {
            Vec::new()
        } else {
            self.parse_closure_params()
        };
        self.expect(&TokenKind::RParen);

        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_expr())
        } else {
            None
        };

        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Closure {
            params,
            return_type,
            body,
            span: self.span_from(start),
        }
    }

    fn parse_closure_params(&mut self) -> Vec<ClosureParam> {
        let mut params = Vec::new();
        params.push(self.parse_closure_param());
        while self.eat(&TokenKind::Comma).is_some() {
            if self.at(&TokenKind::Arrow) {
                break;
            }
            params.push(self.parse_closure_param());
        }
        params
    }

    fn parse_closure_param(&mut self) -> ClosureParam {
        let start = self.current_span();
        match self.peek().clone() {
            TokenKind::Ident(name) if name == "_" => {
                self.advance();
                ClosureParam::Wildcard {
                    span: self.span_from(start),
                }
            }
            TokenKind::Ident(name) => {
                self.advance();
                let type_expr = if self.eat(&TokenKind::Colon).is_some() {
                    Some(self.parse_type_expr())
                } else {
                    None
                };
                ClosureParam::Name {
                    name,
                    type_expr,
                    span: self.span_from(start),
                }
            }
            TokenKind::LParen => {
                self.advance(); // (
                let mut names = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    names.push(self.expect_ident());
                    while self.eat(&TokenKind::Comma).is_some() {
                        names.push(self.expect_ident());
                    }
                }
                self.expect(&TokenKind::RParen);
                ClosureParam::Destructured {
                    names,
                    span: self.span_from(start),
                }
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected closure parameter, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                ClosureParam::Wildcard { span }
            }
        }
    }

    pub(crate) fn expr_to_closure_params(&mut self, expr: &Expr, span: Span) -> Vec<ClosureParam> {
        match expr {
            Expr::Ident { name, span } if name == "_" => {
                vec![ClosureParam::Wildcard { span: *span }]
            }
            Expr::Ident { name, span } => {
                vec![ClosureParam::Name {
                    name: name.clone(),
                    type_expr: None,
                    span: *span,
                }]
            }
            Expr::Tuple { elements, .. } => {
                let mut params = Vec::new();
                for elem in elements {
                    match elem {
                        Expr::Ident { name, span } => {
                            params.push(ClosureParam::Name {
                                name: name.clone(),
                                type_expr: None,
                                span: *span,
                            });
                        }
                        _ => {
                            self.error("invalid closure parameter".to_string(), expr_span(elem));
                            params.push(ClosureParam::Wildcard {
                                span: expr_span(elem),
                            });
                        }
                    }
                }
                params
            }
            Expr::Group { expr: inner, .. } => self.expr_to_closure_params(inner, span),
            _ => {
                self.error("invalid closure parameter list".to_string(), span);
                vec![ClosureParam::Wildcard { span }]
            }
        }
    }

    pub(crate) fn extract_type_path(&self, expr: &Expr) -> Vec<String> {
        match expr {
            Expr::Ident { name, .. } => vec![name.clone()],
            Expr::FieldAccess {
                receiver, field, ..
            } => {
                let mut path = self.extract_type_path(receiver);
                path.push(field.clone());
                path
            }
            _ => vec!["<error>".to_string()],
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

#[cfg(test)]
mod tests {
    use crate::parse;
    use expo_ast::ast::{Expr, Statement, StringPart};

    fn parse_string_parts(source: &str) -> Vec<StringPart> {
        let result = parse(source);
        for item in &result.module.items {
            if let expo_ast::ast::Item::Function(func) = item {
                for stmt in &func.body {
                    if let Statement::Assignment { value, .. } = stmt {
                        if let Expr::String { parts, .. } = value {
                            return parts.clone();
                        }
                    }
                }
            }
        }
        panic!("no string found in parsed output");
    }

    fn literal_values(parts: &[StringPart]) -> Vec<&str> {
        parts
            .iter()
            .filter_map(|p| match p {
                StringPart::Literal { value, .. } => Some(value.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_dedent_basic() {
        let src = "fn main\n  x = \"\"\"\n    hello\n    world\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(literal_values(&parts), vec!["hello\nworld"]);
    }

    #[test]
    fn test_dedent_preserves_extra_indent() {
        let src = "fn main\n  x = \"\"\"\n    line1\n      indented\n    line3\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(literal_values(&parts), vec!["line1\n  indented\nline3"]);
    }

    #[test]
    fn test_dedent_empty_lines_preserved() {
        let src = "fn main\n  x = \"\"\"\n    hello\n\n    world\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(literal_values(&parts), vec!["hello\n\nworld"]);
    }

    #[test]
    fn test_dedent_trailing_newline_stripped() {
        let src = "fn main\n  x = \"\"\"\n    hello\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        let vals = literal_values(&parts);
        assert_eq!(vals, vec!["hello"]);
        assert!(!vals[0].ends_with('\n'));
    }

    #[test]
    fn test_dedent_with_interpolation() {
        let src = "fn main\n  x = \"\"\"\n    hello #{name}\n    world\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(parts.len(), 3);
        match &parts[0] {
            StringPart::Literal { value, .. } => assert_eq!(value, "hello "),
            _ => panic!("expected literal"),
        }
        assert!(matches!(&parts[1], StringPart::Interpolation { .. }));
        match &parts[2] {
            StringPart::Literal { value, .. } => assert_eq!(value, "\nworld"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_dedent_no_indent() {
        let src = "fn main\n  x = \"\"\"\nhello\nworld\n\"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(literal_values(&parts), vec!["hello\nworld"]);
    }

    #[test]
    fn test_multiline_escapes_in_parser() {
        let src = "fn main\n  x = \"\"\"\n    hello\\tworld\n    \"\"\"\nend\n";
        let parts = parse_string_parts(src);
        assert_eq!(literal_values(&parts), vec!["hello\tworld"]);
    }
}
