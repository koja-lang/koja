use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_ast::token::TokenKind;

use crate::parser::Parser;

// Binding powers for Pratt parsing
pub(crate) const BP_ARROW: u8 = 1;
pub(crate) const BP_PIPE_L: u8 = 2;
pub(crate) const BP_PIPE_R: u8 = 3;
pub(crate) const BP_OR_L: u8 = 4;
pub(crate) const BP_OR_R: u8 = 5;
pub(crate) const BP_AND_L: u8 = 6;
pub(crate) const BP_AND_R: u8 = 7;
pub(crate) const BP_NOT_R: u8 = 7;
pub(crate) const BP_CMP_L: u8 = 8;
pub(crate) const BP_CMP_R: u8 = 9;
pub(crate) const BP_ADD_L: u8 = 10;
pub(crate) const BP_ADD_R: u8 = 11;
pub(crate) const BP_MUL_L: u8 = 12;
pub(crate) const BP_MUL_R: u8 = 13;
pub(crate) const BP_UNARY_R: u8 = 15;
pub(crate) const BP_POSTFIX: u8 = 16;

fn infix_bp(kind: &TokenKind) -> Option<(u8, u8)> {
    match kind {
        TokenKind::PipeRight => Some((BP_PIPE_L, BP_PIPE_R)),
        TokenKind::Or => Some((BP_OR_L, BP_OR_R)),
        TokenKind::And => Some((BP_AND_L, BP_AND_R)),
        TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::LtEq
        | TokenKind::GtEq => Some((BP_CMP_L, BP_CMP_R)),
        TokenKind::Plus | TokenKind::Minus => Some((BP_ADD_L, BP_ADD_R)),
        TokenKind::Star | TokenKind::Slash | TokenKind::Percent => Some((BP_MUL_L, BP_MUL_R)),
        _ => None,
    }
}

fn token_to_binop(kind: &TokenKind) -> BinOp {
    match kind {
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Mod,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::NotEq => BinOp::NotEq,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::LtEq => BinOp::LtEq,
        TokenKind::GtEq => BinOp::GtEq,
        TokenKind::And => BinOp::And,
        TokenKind::Or => BinOp::Or,
        TokenKind::PipeRight => BinOp::Pipe,
        _ => unreachable!("not a binary operator: {:?}", kind),
    }
}

impl Parser {
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    pub(crate) fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_prefix();

        loop {
            // Check for short closure arrow
            if matches!(self.peek(), TokenKind::Arrow) && min_bp <= BP_ARROW {
                self.advance(); // ->
                let params = self.expr_to_closure_params(&lhs, expr_span(&lhs));
                let body = self.parse_expr_bp(BP_ARROW);
                let span = Span::new(expr_span(&lhs).start, expr_span(&body).end);
                lhs = Expr::ShortClosure {
                    params,
                    body: Box::new(body),
                    span,
                };
                continue;
            }

            // Postfix operators (highest precedence)
            if BP_POSTFIX >= min_bp {
                match self.peek() {
                    TokenKind::Dot => {
                        self.advance(); // .
                        match self.peek().clone() {
                            TokenKind::Ident(name) => {
                                self.advance();
                                if self.at(&TokenKind::LParen) {
                                    // Method call
                                    self.advance(); // (
                                    let args = self.parse_arg_list();
                                    self.expect(&TokenKind::RParen);
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::MethodCall {
                                        receiver: Box::new(lhs),
                                        method: name,
                                        type_args: None,
                                        args,
                                        span,
                                    };
                                } else {
                                    // Field access
                                    let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                    lhs = Expr::FieldAccess {
                                        receiver: Box::new(lhs),
                                        field: name,
                                        span,
                                    };
                                }
                                continue;
                            }
                            TokenKind::TypeIdent(variant) | TokenKind::ConstIdent(variant) => {
                                self.advance();
                                let type_path = self.extract_type_path(&lhs);
                                lhs = self.parse_enum_construction_tail(
                                    type_path,
                                    variant,
                                    expr_span(&lhs),
                                );
                                continue;
                            }
                            _ => {
                                let span = self.current_span();
                                self.error(
                                    format!(
                                        "expected field name or variant after '.', found {:?}",
                                        self.peek()
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                    TokenKind::LParen => {
                        // Function call
                        self.advance(); // (
                        let args = self.parse_arg_list();
                        self.expect(&TokenKind::RParen);
                        let span = Span::new(expr_span(&lhs).start, self.prev_end());
                        lhs = Expr::Call {
                            callee: Box::new(lhs),
                            type_args: None,
                            args,
                            span,
                        };
                        continue;
                    }
                    TokenKind::Question => {
                        let span = Span::new(expr_span(&lhs).start, self.current_span().end);
                        self.advance(); // ?
                        lhs = Expr::Try {
                            expr: Box::new(lhs),
                            span,
                        };
                        continue;
                    }
                    TokenKind::ColonColon => {
                        // Turbofish: expr::<Type>(args)
                        if matches!(self.peek_nth(1), TokenKind::Lt) {
                            self.advance(); // ::
                            self.advance(); // <
                            let mut type_args = vec![self.parse_type_expr()];
                            while self.eat(&TokenKind::Comma).is_some() {
                                type_args.push(self.parse_type_expr());
                            }
                            self.expect(&TokenKind::Gt);

                            if self.eat(&TokenKind::LParen).is_some() {
                                let args = self.parse_arg_list();
                                self.expect(&TokenKind::RParen);
                                let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                lhs = Expr::Call {
                                    callee: Box::new(lhs),
                                    type_args: Some(type_args),
                                    args,
                                    span,
                                };
                            } else {
                                let span = Span::new(expr_span(&lhs).start, self.prev_end());
                                lhs = Expr::Call {
                                    callee: Box::new(lhs),
                                    type_args: Some(type_args),
                                    args: Vec::new(),
                                    span,
                                };
                            }
                            continue;
                        }
                    }
                    _ => {}
                }
            }

            // Infix operators
            if let Some((l_bp, r_bp)) = infix_bp(self.peek()) {
                if l_bp < min_bp {
                    break;
                }
                let op_token = self.advance();
                let op = token_to_binop(&op_token.kind);
                let rhs = self.parse_expr_bp(r_bp);
                let span = Span::new(expr_span(&lhs).start, expr_span(&rhs).end);
                lhs = Expr::Binary {
                    op,
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                    span,
                };
                continue;
            }

            break;
        }

        lhs
    }

    // =========================================================================
    // Prefix parsing
    // =========================================================================

    fn parse_prefix(&mut self) -> Expr {
        match self.peek().clone() {
            // Literals
            TokenKind::IntLit(s) => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Int(s),
                    span: self.span_from(start),
                }
            }
            TokenKind::FloatLit(s) => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Float(s),
                    span: self.span_from(start),
                }
            }
            TokenKind::True => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Bool(true),
                    span: self.span_from(start),
                }
            }
            TokenKind::False => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Bool(false),
                    span: self.span_from(start),
                }
            }
            TokenKind::None_ => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::None,
                    span: self.span_from(start),
                }
            }

            // String
            TokenKind::StringStart => self.parse_string_expr(false),

            // Identifiers
            TokenKind::Ident(name) => {
                let start = self.current_span();
                self.advance();
                Expr::Ident {
                    name,
                    span: self.span_from(start),
                }
            }

            // Type identifiers (struct/enum construction)
            TokenKind::TypeIdent(_) | TokenKind::ConstIdent(_) => self.parse_type_construction(),

            // Self
            TokenKind::Self_ => {
                let start = self.current_span();
                self.advance();
                Expr::Self_ {
                    span: self.span_from(start),
                }
            }

            // Grouping / Tuple / Unit
            TokenKind::LParen => self.parse_paren_expr(),

            // List literal
            TokenKind::LBracket => self.parse_list_expr(),

            // Unary minus
            TokenKind::Minus => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_R);
                Expr::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                    span: self.span_from(start),
                }
            }

            // Logical not
            TokenKind::Not => {
                let start = self.current_span();
                self.advance();
                let operand = self.parse_expr_bp(BP_NOT_R);
                Expr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                    span: self.span_from(start),
                }
            }

            // Control flow
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Unless => self.parse_unless_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Cond => self.parse_cond_expr(),
            TokenKind::For => self.parse_for_expr(),
            TokenKind::Loop => self.parse_loop_expr(),

            // Arena
            TokenKind::Arena => self.parse_arena_expr(),

            // Await
            TokenKind::Await => self.parse_await_expr(),

            // Receive
            TokenKind::Receive => self.parse_receive_expr(),

            // Spawn
            TokenKind::Spawn => self.parse_spawn_expr(),

            // Closure
            TokenKind::Fn => self.parse_closure_expr(),

            TokenKind::Break => {
                let start = self.current_span();
                self.advance();
                Expr::Literal {
                    value: Literal::Unit,
                    span: self.span_from(start),
                }
            }

            TokenKind::Return => {
                let start = self.current_span();
                self.advance();
                let value = if matches!(
                    self.peek(),
                    TokenKind::Newline | TokenKind::End | TokenKind::Eof | TokenKind::RParen
                ) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                // Wrap in a literal unit placeholder -- the statement layer handles Return properly
                // but expressions sometimes have return in tail position
                if let Some(val) = value {
                    val
                } else {
                    Expr::Literal {
                        value: Literal::Unit,
                        span: self.span_from(start),
                    }
                }
            }

            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected expression, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                Expr::Literal {
                    value: Literal::Unit,
                    span,
                }
            }
        }
    }

    // =========================================================================
    // Specific expression parsers
    // =========================================================================

    fn parse_string_expr(&mut self, _multiline: bool) -> Expr {
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
                    let format = None; // TODO: parse format spec
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

    fn parse_type_construction(&mut self) -> Expr {
        let start = self.current_span();
        let first = self.expect_type_ident();
        let mut path = vec![first];

        while self.at(&TokenKind::Dot) {
            if matches!(
                self.peek_nth(1),
                TokenKind::TypeIdent(_) | TokenKind::ConstIdent(_)
            ) {
                // Could be Type.Type (path) or Type.Variant (enum construction)
                // Peek ahead to see if there's a { or ( after
                self.advance(); // .
                let seg = self.expect_type_ident();

                // If next is { or (, this segment might be the variant
                // But we need to continue collecting the path
                if self.at(&TokenKind::LBrace)
                    || self.at(&TokenKind::LParen)
                    || self.at(&TokenKind::Dot)
                {
                    // Check if next after this is another .TypeIdent
                    if self.at(&TokenKind::Dot)
                        && matches!(
                            self.peek_nth(1),
                            TokenKind::TypeIdent(_) | TokenKind::ConstIdent(_)
                        )
                    {
                        path.push(seg);
                        continue;
                    }
                    // This is a variant or struct construction
                    return self.parse_enum_construction_tail(path, seg, start);
                } else {
                    // Enum unit construction: Type.Variant (no parens or braces)
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

        // Struct construction: Type{field: value, ...}
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
            // Could be a function call on a type: Type(args)
            // This handles constructors like Ok(value), Err(value), Some(value)
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
            // Just a type name as an expression (e.g., referring to a type)
            Expr::Ident {
                name: path.join("."),
                span: self.span_from(start),
            }
        }
    }

    fn parse_enum_construction_tail(
        &mut self,
        type_path: Vec<String>,
        variant: String,
        start: Span,
    ) -> Expr {
        if self.eat(&TokenKind::LParen).is_some() {
            // Tuple enum construction: Type.Variant(expr, ...)
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
            // Struct enum construction: Type.Variant{field: value, ...}
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

    fn parse_paren_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // (

        // Unit: ()
        if self.eat(&TokenKind::RParen).is_some() {
            return Expr::Literal {
                value: Literal::Unit,
                span: self.span_from(start),
            };
        }

        let first = self.parse_expr();

        if self.eat(&TokenKind::Comma).is_some() {
            // Tuple: (a, b, ...)
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
            // Grouping: (expr)
            self.expect(&TokenKind::RParen);
            Expr::Group {
                expr: Box::new(first),
                span: self.span_from(start),
            }
        }
    }

    fn parse_list_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // [

        let mut elements = Vec::new();
        if !self.at(&TokenKind::RBracket) {
            elements.push(self.parse_expr());
            while self.eat(&TokenKind::Comma).is_some() {
                if self.at(&TokenKind::RBracket) {
                    break;
                }
                elements.push(self.parse_expr());
            }
        }
        self.expect(&TokenKind::RBracket);

        Expr::List {
            elements,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Control flow
    // =========================================================================

    fn parse_if_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // if

        let condition = self.parse_expr();
        let then_body = self.parse_block();

        let else_body = if self.eat(&TokenKind::Else).is_some() {
            Some(self.parse_block())
        } else {
            None
        };
        self.expect(&TokenKind::End);

        Expr::If {
            condition: Box::new(condition),
            then_body,
            else_body,
            span: self.span_from(start),
        }
    }

    fn parse_unless_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // unless

        let condition = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Unless {
            condition: Box::new(condition),
            body,
            span: self.span_from(start),
        }
    }

    fn parse_match_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // match

        let subject = self.parse_expr();
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            arms.push(self.parse_match_arm());
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Match {
            subject: Box::new(subject),
            arms,
            span: self.span_from(start),
        }
    }

    fn parse_match_arm(&mut self) -> MatchArm {
        let start = self.current_span();
        let pattern = self.parse_pattern();

        let guard = if self.eat(&TokenKind::When).is_some() {
            // Parse at BP above arrow so -> terminates the guard expression
            Some(self.parse_expr_bp(BP_PIPE_L))
        } else {
            None
        };

        self.expect(&TokenKind::Arrow);
        let body = self.parse_match_body();

        MatchArm {
            pattern,
            guard,
            body,
            span: self.span_from(start),
        }
    }

    fn parse_match_body(&mut self) -> Vec<Statement> {
        let mut stmts = Vec::new();

        // Parse one statement on the same line as ->
        if !matches!(
            self.peek(),
            TokenKind::End | TokenKind::Eof | TokenKind::Newline
        ) {
            stmts.push(self.parse_statement());
        }

        self.skip_newlines();

        // Multi-line arm body: keep parsing until we see something that looks
        // like a new match arm (pattern -> ...) or `end`
        while !self.at(&TokenKind::End) && !self.at_eof() {
            if self.looks_like_new_arm() {
                break;
            }
            let before = self.pos;
            stmts.push(self.parse_statement());
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }

        stmts
    }

    /// Heuristic: does the current position look like the start of a new
    /// match/cond/receive arm rather than a continuation statement?
    /// We scan forward to see if there's a `->` before a newline/end.
    fn looks_like_new_arm(&self) -> bool {
        let mut i = self.pos;
        let mut depth = 0u32;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::Arrow if depth == 0 => return true,
                TokenKind::Newline | TokenKind::End | TokenKind::Eof if depth == 0 => {
                    return false;
                }
                TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => {
                    depth += 1;
                }
                TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn parse_cond_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // cond
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            let arm_start = self.current_span();
            let condition = self.parse_expr();
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body();
            arms.push(CondArm {
                condition,
                body,
                span: self.span_from(arm_start),
            });
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Cond {
            arms,
            span: self.span_from(start),
        }
    }

    fn parse_for_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // for

        let pattern = self.parse_pattern();
        self.expect(&TokenKind::In);
        let iterable = self.parse_expr();
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::For {
            pattern,
            iterable: Box::new(iterable),
            body,
            span: self.span_from(start),
        }
    }

    fn parse_loop_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // loop
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Loop {
            body,
            span: self.span_from(start),
        }
    }

    fn parse_arena_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // arena
        let body = self.parse_block();
        self.expect(&TokenKind::End);

        Expr::Arena {
            body,
            span: self.span_from(start),
        }
    }

    fn parse_await_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // await
        let expr = self.parse_expr();

        Expr::Await {
            expr: Box::new(expr),
            span: self.span_from(start),
        }
    }

    fn parse_receive_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // receive
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.at(&TokenKind::End) && !self.at_eof() {
            let before = self.pos;
            let arm_start = self.current_span();
            let pattern = self.parse_pattern();
            self.expect(&TokenKind::Eq);
            // Parse source at BP above arrow so -> is not consumed as short closure
            let source = self.parse_expr_bp(BP_PIPE_L);
            self.expect(&TokenKind::Arrow);
            let body = self.parse_match_body();
            arms.push(ReceiveArm {
                pattern,
                source,
                body,
                span: self.span_from(arm_start),
            });
            if self.pos == before {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(&TokenKind::End);

        Expr::Receive {
            arms,
            span: self.span_from(start),
        }
    }

    fn parse_spawn_expr(&mut self) -> Expr {
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

    fn parse_closure_expr(&mut self) -> Expr {
        let start = self.current_span();
        self.advance(); // fn

        // Parse closure params (before ->)
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

    // =========================================================================
    // Argument list
    // =========================================================================

    pub(crate) fn parse_arg_list(&mut self) -> Vec<Arg> {
        let mut args = Vec::new();
        if self.at(&TokenKind::RParen) {
            return args;
        }

        args.push(self.parse_arg());
        while self.eat(&TokenKind::Comma).is_some() {
            if self.at(&TokenKind::RParen) {
                break;
            }
            args.push(self.parse_arg());
        }
        args
    }

    fn parse_arg(&mut self) -> Arg {
        let start = self.current_span();

        // Check for keyword argument: ident: expr
        if let TokenKind::Ident(name) = self.peek().clone() {
            if matches!(self.peek_nth(1), TokenKind::Colon) {
                // But only if the thing after colon looks like an expression, not a type
                // Peek at position 2: if it's a type-like token, this might not be a keyword arg
                // Actually in call context, name: expr is always a keyword arg
                self.advance(); // ident
                self.advance(); // :
                let value = self.parse_expr();
                return Arg {
                    name: Some(name),
                    value,
                    span: self.span_from(start),
                };
            }
        }

        let value = self.parse_expr();
        Arg {
            name: None,
            value,
            span: self.span_from(start),
        }
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn expr_to_closure_params(&mut self, expr: &Expr, span: Span) -> Vec<ClosureParam> {
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
            Expr::Group { expr: inner, .. } => {
                // (x) -> expr, treat as single param
                self.expr_to_closure_params(inner, span)
            }
            _ => {
                self.error("invalid closure parameter list".to_string(), span);
                vec![ClosureParam::Wildcard { span }]
            }
        }
    }

    fn extract_type_path(&self, expr: &Expr) -> Vec<String> {
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

    pub(crate) fn parse_literal_value(&mut self) -> Literal {
        match self.peek().clone() {
            TokenKind::IntLit(s) => {
                self.advance();
                Literal::Int(s)
            }
            TokenKind::FloatLit(s) => {
                self.advance();
                Literal::Float(s)
            }
            TokenKind::True => {
                self.advance();
                Literal::Bool(true)
            }
            TokenKind::False => {
                self.advance();
                Literal::Bool(false)
            }
            TokenKind::None_ => {
                self.advance();
                Literal::None
            }
            _ => {
                self.error(
                    format!("expected literal, found {:?}", self.peek()),
                    self.current_span(),
                );
                Literal::None
            }
        }
    }
}

pub(crate) fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Arena { span, .. }
        | Expr::Await { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Cond { span, .. }
        | Expr::EnumConstruction { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::For { span, .. }
        | Expr::Group { span, .. }
        | Expr::Ident { span, .. }
        | Expr::If { span, .. }
        | Expr::List { span, .. }
        | Expr::Literal { span, .. }
        | Expr::Loop { span, .. }
        | Expr::Match { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Receive { span, .. }
        | Expr::Self_ { span, .. }
        | Expr::ShortClosure { span, .. }
        | Expr::Spawn { span, .. }
        | Expr::String { span, .. }
        | Expr::StructConstruction { span, .. }
        | Expr::Try { span, .. }
        | Expr::Tuple { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Unless { span, .. } => *span,
    }
}
