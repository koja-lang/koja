use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_ast::token::TokenKind;

use crate::expr::expr_span;
use crate::parser::Parser;

impl Parser {
    pub(crate) fn parse_string_expr(&mut self, _multiline: bool) -> Expr {
        let start = self.current_span();
        self.advance(); // StringStart or MultilineStringStart

        let mut parts = Vec::new();
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
                    let format = None;
                    self.expect(&TokenKind::InterpolEnd);
                    parts.push(StringPart::Interpolation {
                        expr,
                        format,
                        span: self.span_from(interp_start),
                    });
                }
                TokenKind::StringEnd | TokenKind::MultilineStringEnd => {
                    self.advance();
                    break;
                }
                _ => {
                    self.error("unterminated string".to_string(), self.current_span());
                    break;
                }
            }
        }

        if parts.is_empty() {
            Expr::String {
                parts: vec![StringPart::Literal {
                    value: String::new(),
                    span: self.span_from(start),
                }],
                multiline: _multiline,
                span: self.span_from(start),
            }
        } else {
            Expr::String {
                parts,
                multiline: _multiline,
                span: self.span_from(start),
            }
        }
    }

    pub(crate) fn parse_type_construction(&mut self) -> Expr {
        let start = self.current_span();
        let first = self.expect_type_ident();
        let mut path = vec![first];

        while self.at(&TokenKind::Dot) {
            if matches!(
                self.peek_nth(1),
                TokenKind::TypeIdent(_) | TokenKind::ConstIdent(_)
            ) {
                self.advance(); // .
                let seg = self.expect_type_ident();

                if self.at(&TokenKind::LBrace)
                    || self.at(&TokenKind::LParen)
                    || self.at(&TokenKind::Dot)
                {
                    if self.at(&TokenKind::Dot)
                        && matches!(
                            self.peek_nth(1),
                            TokenKind::TypeIdent(_) | TokenKind::ConstIdent(_)
                        )
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
                if self.eat(&TokenKind::Comma).is_none() {
                    self.skip_newlines();
                } else {
                    self.skip_newlines();
                }
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
                type_args: None,
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
                if self.eat(&TokenKind::Comma).is_none() {
                    self.skip_newlines();
                } else {
                    self.skip_newlines();
                }
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
            Expr::Tuple {
                elements,
                span: self.span_from(start),
            }
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

        let mut elements = Vec::new();
        self.skip_newlines();
        if !self.at(&TokenKind::RBracket) {
            elements.push(self.parse_expr());
            while self.eat(&TokenKind::Comma).is_some() {
                self.skip_newlines();
                if self.at(&TokenKind::RBracket) {
                    break;
                }
                elements.push(self.parse_expr());
            }
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
        self.expect(&TokenKind::LParen);
        let expr = self.parse_expr();
        self.expect(&TokenKind::RParen);

        Expr::Spawn {
            expr: Box::new(expr),
            span: self.span_from(start),
        }
    }

    pub(crate) fn parse_closure_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // fn

        let params = if self.at(&TokenKind::Arrow) {
            Vec::new()
        } else {
            self.parse_closure_params()
        };

        self.expect(&TokenKind::Arrow);
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Closure {
            params,
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
                ClosureParam::Name {
                    name,
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
