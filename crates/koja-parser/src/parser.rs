use koja_ast::ast::{Comment, Diagnostic, File, Item, Severity, Statement, Visibility};
use koja_ast::span::{Position, Span};
use koja_ast::token::{Token, TokenKind};
use koja_lexer::{LexResult, lex};

/// Sentinel name emitted in place of a missing identifier so the
/// parser can keep building an AST after a diagnostic. Downstream
/// passes treat this as opaque text and never resolve it; it shows
/// up only inside error-recovery paths.
pub(crate) const ERROR_IDENT: &str = "<error>";

/// Selects the top-level grammar accepted by the parser.
///
/// `ParseMode::File` (default) parses today's `.koja` compilation-unit
/// grammar: only declarations (`fn`, `struct`, `enum`, `protocol`,
/// `impl`, `const`, ...) at the top level. `File.body` is always
/// `None` in this mode.
///
/// `ParseMode::Script` additionally accepts top-level statements (bare
/// expressions, assignments, etc.) interleaved with declarations.
/// Statements collect into `File.body = Some(...)` and stay there
/// through typecheck; downstream passes (`koja-ir::lower_script`)
/// consume the body directly. There is no synthetic `fn main` wrapper.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ParseMode {
    #[default]
    File,
    Script,
}

pub struct ParseResult {
    pub ast: File,
    pub errors: Vec<Diagnostic>,
}

pub(crate) struct Parser {
    pub(crate) tokens: Vec<Token>,
    pub(crate) comments: Vec<Comment>,
    pub(crate) pos: usize,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) pending_token: Option<TokenKind>,
}

/// Snapshot of `Parser` state for speculative parsing. Captured by
/// [`Parser::save_pos`] before a tentative read and restored via
/// [`Parser::restore_pos`] when the speculation aborts. Restoring
/// also truncates any diagnostics emitted during the discarded
/// branch.
#[derive(Clone, Copy)]
pub(crate) struct Checkpoint {
    pos: usize,
    error_count: usize,
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

    /// Matches a specific lowercase identifier in argument position
    /// (used by the binary-segment grammar for `byte`, `signed`,
    /// `unsigned`, `big`, `little`). These are not reserved
    /// keywords, just recognized in the segment-suffix position.
    pub(crate) fn at_contextual_ident(&self, name: &str) -> bool {
        matches!(self.peek(), TokenKind::Ident(n) if n == name)
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

    /// Generic "arm loop": parse items until `should_stop` fires
    /// or we hit EOF. Each iteration verifies forward progress and
    /// emits an `unexpected token` diagnostic + recovery advance
    /// when the inner parser stalls, so a malformed item doesn't
    /// wedge the entire block. Trailing newlines between items are
    /// trimmed for the caller.
    ///
    /// Used by `match`, `cond`, `receive`, and plain blocks.
    pub(crate) fn parse_until<F, P, T>(&mut self, mut should_stop: F, mut parse_one: P) -> Vec<T>
    where
        F: FnMut(&Self) -> bool,
        P: FnMut(&mut Self) -> T,
    {
        let mut items = Vec::new();
        while !should_stop(self) && !self.at_eof() {
            let before = self.pos;
            items.push(parse_one(self));
            if self.pos == before {
                self.error(
                    format!("unexpected token {:?}", self.peek()),
                    self.current_span(),
                );
                self.advance();
            }
            self.skip_newlines();
        }
        items
    }

    /// Parse a comma-separated sequence of items terminated by
    /// `terminator`. The opening delimiter must already be consumed
    /// by the caller; this helper:
    ///
    /// - skips newlines before the first item
    /// - returns `Vec::new()` immediately if the terminator is
    ///   already in front of us (empty sequence)
    /// - tolerates a trailing comma before the terminator
    /// - skips newlines between items
    ///
    /// The terminator itself is **not** consumed — the caller still
    /// owns the `expect(&terminator)` (or equivalent) that closes
    /// the construct, because some constructs want to attach a
    /// custom error to the missing terminator.
    pub(crate) fn comma_separated<T>(
        &mut self,
        terminator: &TokenKind,
        mut parse_one: impl FnMut(&mut Self) -> T,
    ) -> Vec<T> {
        self.skip_newlines();
        if self.at(terminator) {
            return Vec::new();
        }
        let mut items = vec![parse_one(self)];
        while self.eat(&TokenKind::Comma).is_some() {
            self.skip_newlines();
            if self.at(terminator) {
                break;
            }
            items.push(parse_one(self));
        }
        self.skip_newlines();
        items
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
                ERROR_IDENT.to_string()
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
                ERROR_IDENT.to_string()
            }
        }
    }

    pub(crate) fn save_pos(&self) -> Checkpoint {
        Checkpoint {
            pos: self.pos,
            error_count: self.errors.len(),
        }
    }

    pub(crate) fn restore_pos(&mut self, saved: Checkpoint) {
        self.pos = saved.pos;
        self.errors.truncate(saved.error_count);
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

    pub(crate) fn parse_file(&mut self, mode: ParseMode) -> File {
        let start = self.current_span();
        let mut items = Vec::new();
        let mut body: Vec<Statement> = Vec::new();

        self.skip_newlines();
        while !self.at_eof() {
            match mode {
                ParseMode::File => {
                    if let Some(item) = self.parse_item() {
                        items.push(item);
                    }
                }
                ParseMode::Script => {
                    if self.at_top_level_item_starter() {
                        if let Some(item) = self.parse_item() {
                            items.push(item);
                        }
                    } else {
                        body.push(self.parse_statement());
                    }
                }
            }
            self.skip_newlines();
        }

        // Script-mode body is only `Some` when there's actually
        // something to hoist. An empty `body` (`fn main; ... end`
        // parsed in script mode) collapses to `None` so downstream
        // passes can tell "items-only script" apart from "script with
        // top-level statements" purely by the body shape.
        let body = match mode {
            ParseMode::File => None,
            ParseMode::Script if body.is_empty() => None,
            ParseMode::Script => Some(body),
        };

        File {
            body,
            comments: self.comments.clone(),
            items,
            package: String::new(),
            path: None,
            span: self.span_from(start),
        }
    }

    /// Returns `true` when the current token (and, for `fn`, the
    /// following one) starts a top-level declaration as opposed to a
    /// statement. Used by `ParseMode::Script` to dispatch between
    /// `parse_item` and `parse_statement` at the top level.
    ///
    /// `fn` is ambiguous: `fn name(...) ... end` is a function item,
    /// `fn(x) -> x + 1` is a closure expression. The disambiguator is
    /// the token immediately after `fn` -- an identifier means item,
    /// `(` means closure expression. The `@` annotation prefix is
    /// treated as an item starter because the only legal followers are
    /// declaration keywords (struct/enum/fn/...).
    pub(crate) fn at_top_level_item_starter(&self) -> bool {
        match self.peek() {
            TokenKind::Alias
            | TokenKind::At
            | TokenKind::Const
            | TokenKind::Enum
            | TokenKind::Extend
            | TokenKind::Impl
            | TokenKind::Priv
            | TokenKind::Protocol
            | TokenKind::Struct
            | TokenKind::Type => true,
            TokenKind::Fn => matches!(self.peek_nth(1), TokenKind::Ident(_)),
            _ => false,
        }
    }

    /// Parse a single top-level item. Annotations (`@name [value]`)
    /// are read once up front so the dispatch table below is flat:
    /// every declaration kind gets the (possibly empty) annotation
    /// list directly. `impl` and `alias` decline annotations — if
    /// any are present we route to the "annotation must be followed
    /// by a declaration" diagnostic instead.
    fn parse_item(&mut self) -> Option<Item> {
        self.skip_newlines();
        let annotations = self.parse_annotations();
        self.skip_newlines();

        match self.peek().clone() {
            TokenKind::Struct => Some(self.parse_struct_item(annotations)),
            TokenKind::Enum => Some(self.parse_enum_item(annotations)),
            TokenKind::Protocol => Some(self.parse_protocol_item(annotations)),
            TokenKind::Fn => Some(self.parse_function_item(annotations, Visibility::Public)),
            TokenKind::Priv => {
                self.advance();
                Some(self.parse_function_item(annotations, Visibility::Private))
            }
            TokenKind::Const => Some(self.parse_constant_item(annotations)),
            TokenKind::Type => Some(self.parse_type_alias_item(annotations)),
            TokenKind::Impl if annotations.is_empty() => Some(self.parse_impl_item()),
            TokenKind::Extend if annotations.is_empty() => Some(self.parse_extend_item()),
            TokenKind::Alias if annotations.is_empty() => Some(self.parse_alias_item()),
            _ if !annotations.is_empty() => {
                let span = self.current_span();
                self.error(
                    "annotation must be followed by a declaration".to_string(),
                    span,
                );
                None
            }
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

pub fn parse(source: &str, mode: ParseMode) -> ParseResult {
    let lex_result = lex(source);
    let mut parser = Parser::new(lex_result);
    let ast = parser.parse_file(mode);
    ParseResult {
        ast,
        errors: parser.errors,
    }
}
