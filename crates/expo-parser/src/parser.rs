use expo_ast::ast::{Comment, Diagnostic, Item, Module, Severity, Visibility};
use expo_ast::span::{Position, Span};
use expo_ast::token::{Token, TokenKind};
use expo_lexer::{LexResult, lex};

pub struct ParseResult {
    pub module: Module,
    pub errors: Vec<Diagnostic>,
}

pub(crate) struct Parser {
    pub(crate) tokens: Vec<Token>,
    pub(crate) comments: Vec<Comment>,
    pub(crate) pos: usize,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) pending_token: Option<TokenKind>,
}

impl Parser {
    pub(crate) fn new(lex_result: LexResult) -> Self {
        Self {
            tokens: lex_result.tokens,
            comments: lex_result.comments,
            pos: 0,
            errors: lex_result.errors,
            pending_token: None,
        }
    }

    // =========================================================================
    // Token navigation
    // =========================================================================

    pub(crate) fn peek(&self) -> &TokenKind {
        if let Some(ref pt) = self.pending_token {
            pt
        } else {
            &self.tokens[self.pos].kind
        }
    }

    pub(crate) fn peek_nth(&self, n: usize) -> &TokenKind {
        self.tokens
            .get(self.pos + n)
            .map(|t| &t.kind)
            .unwrap_or(&TokenKind::EndOfFile)
    }

    pub(crate) fn current_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    pub(crate) fn advance(&mut self) -> Token {
        if let Some(pt) = self.pending_token.take() {
            return Token {
                kind: pt,
                span: self.tokens[self.pos].span,
            };
        }
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    pub(crate) fn at(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(kind)
    }

    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::EndOfFile)
    }

    pub(crate) fn eat(&mut self, kind: &TokenKind) -> Option<Token> {
        if self.at(kind) {
            Some(self.advance())
        } else {
            None
        }
    }

    pub(crate) fn expect(&mut self, kind: &TokenKind) -> Token {
        if self.at(kind) {
            self.advance()
        } else {
            let span = self.current_span();
            let message = format!("expected {kind:?}, found {:?}", self.peek());
            let hint = match kind {
                TokenKind::End => Some("every 'fn', 'if', 'while', 'loop', 'for', and 'struct' must be closed with 'end'".into()),
                _ => None,
            };
            self.errors.push(Diagnostic {
                severity: Severity::Error,
                message,
                hint,
                span,
            });
            self.advance()
        }
    }

    /// Expect a closing `>` for generics. Handles the `>>` ambiguity:
    /// if the current token is `>>`, consumes it and stashes a leftover `>`
    /// for the outer generic to pick up on the next call.
    pub(crate) fn expect_gt(&mut self) {
        if self.pending_token == Some(TokenKind::Gt) {
            self.pending_token = None;
        } else if self.eat(&TokenKind::Gt).is_some() {
            // Normal single > in the token stream.
        } else if self.eat(&TokenKind::GtGt).is_some() {
            self.pending_token = Some(TokenKind::Gt);
        } else {
            let span = self.current_span();
            self.error(format!("expected >, found {:?}", self.peek()), span);
        }
    }

    pub(crate) fn expect_ident(&mut self) -> String {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                name
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected identifier, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                String::from("<error>")
            }
        }
    }

    /// Accept a TypeIdent token and return its name.
    pub(crate) fn expect_type_ident(&mut self) -> String {
        match self.peek().clone() {
            TokenKind::TypeIdent(name) => {
                self.advance();
                name
            }
            _ => {
                let span = self.current_span();
                self.error(
                    format!("expected type identifier, found {:?}", self.peek()),
                    span,
                );
                self.advance();
                String::from("<error>")
            }
        }
    }

    pub(crate) fn save_pos(&self) -> (usize, usize) {
        (self.pos, self.errors.len())
    }

    pub(crate) fn restore_pos(&mut self, saved: (usize, usize)) {
        self.pos = saved.0;
        self.errors.truncate(saved.1);
    }

    pub(crate) fn skip_newlines(&mut self) {
        while matches!(self.peek(), TokenKind::Newline) {
            self.advance();
        }
    }

    pub(crate) fn span_from(&self, start: Span) -> Span {
        Span::new(start.start, self.prev_end())
    }

    pub(crate) fn prev_end(&self) -> Position {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span.end
        } else {
            Position {
                offset: 0,
                line: 1,
                column: 1,
            }
        }
    }

    // =========================================================================
    // Error handling
    // =========================================================================

    pub(crate) fn error(&mut self, message: String, span: Span) {
        self.errors.push(Diagnostic {
            severity: Severity::Error,
            message,
            hint: None,
            span,
        });
    }

    pub(crate) fn error_with_hint(&mut self, message: String, hint: String, span: Span) {
        self.errors.push(Diagnostic {
            severity: Severity::Error,
            message,
            hint: Some(hint),
            span,
        });
    }

    // =========================================================================
    // Top-level parsing
    // =========================================================================

    pub(crate) fn parse_module(&mut self) -> Module {
        let start = self.current_span();
        let mut items = Vec::new();

        self.skip_newlines();
        while !self.at_eof() {
            if let Some(item) = self.parse_item() {
                items.push(item);
            }
            self.skip_newlines();
        }

        Module {
            items,
            comments: self.comments.clone(),
            span: self.span_from(start),
            path: None,
        }
    }

    fn parse_item(&mut self) -> Option<Item> {
        self.skip_newlines();
        match self.peek().clone() {
            TokenKind::Struct => Some(self.parse_struct_item()),
            TokenKind::Enum => Some(self.parse_enum_item()),
            TokenKind::Protocol => Some(self.parse_protocol_item(None)),
            TokenKind::Impl => Some(self.parse_impl_item()),
            TokenKind::Fn => Some(self.parse_function_item(None, Visibility::Public)),
            TokenKind::Priv => {
                self.advance();
                Some(self.parse_function_item(None, Visibility::Private))
            }
            TokenKind::At => {
                let annotation = self.parse_annotation();
                self.skip_newlines();
                match self.peek().clone() {
                    TokenKind::Struct => {
                        Some(self.parse_struct_item_with_annotation(Some(annotation)))
                    }
                    TokenKind::Enum => Some(self.parse_enum_item_with_annotation(Some(annotation))),
                    TokenKind::Protocol => Some(self.parse_protocol_item(Some(annotation))),
                    TokenKind::Fn => {
                        Some(self.parse_function_item(Some(annotation), Visibility::Public))
                    }
                    TokenKind::Priv => {
                        self.advance();
                        Some(self.parse_function_item(Some(annotation), Visibility::Private))
                    }
                    TokenKind::Const => Some(self.parse_constant_item(Some(annotation))),
                    TokenKind::Type => Some(self.parse_type_alias_item(Some(annotation))),
                    _ => {
                        let span = self.current_span();
                        self.error(
                            "annotation must be followed by a declaration".to_string(),
                            span,
                        );
                        None
                    }
                }
            }
            TokenKind::Type => Some(self.parse_type_alias_item(None)),
            TokenKind::Alias => Some(self.parse_alias_item()),
            TokenKind::Shared => Some(self.parse_shared_item()),
            TokenKind::Const => Some(self.parse_constant_item(None)),
            _ => {
                let span = self.current_span();
                self.error(
                    format!("unexpected token at top level: {:?}", self.peek()),
                    span,
                );
                self.advance();
                None
            }
        }
    }
}

pub fn parse(source: &str) -> ParseResult {
    let lex_result = lex(source);
    let mut parser = Parser::new(lex_result);
    let module = parser.parse_module();
    ParseResult {
        module,
        errors: parser.errors,
    }
}
