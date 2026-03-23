use expo_ast::ast::{Diagnostic, Severity};

use crate::{Comment, Position, Span, Token, TokenKind};

/// The output of lexing: tokens, extracted comments, and any lexical errors.
#[derive(Debug)]
pub struct LexResult {
    /// Source comments, in order of appearance.
    pub comments: Vec<Comment>,
    /// Lexical errors (unterminated strings, unknown escapes, etc.).
    pub errors: Vec<Diagnostic>,
    /// The token stream, always terminated by `TokenKind::Eof`.
    pub tokens: Vec<Token>,
}

/// Whether a string literal is single-line or triple-quoted multiline.
#[derive(Debug, Clone, Copy, PartialEq)]
enum StringMode {
    Single,
    Multiline,
}

/// Tracks brace nesting depth inside a string interpolation so we know
/// when a `}` closes the interpolation vs. a nested expression.
#[derive(Debug)]
struct InterpolState {
    mode: StringMode,
    brace_depth: u32,
}

/// Mutable state for the lexer: character buffer, cursor, and output vectors.
struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    column: u32,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    errors: Vec<Diagnostic>,
    string_stack: Vec<InterpolState>,
}

/// Tokenizes Expo source code into a stream of tokens, comments, and errors.
pub fn lex(source: &str) -> LexResult {
    let mut lexer = Lexer::new(source);
    lexer.run();
    LexResult {
        tokens: lexer.tokens,
        comments: lexer.comments,
        errors: lexer.errors,
    }
}

impl Lexer {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            column: 1,
            tokens: Vec::new(),
            comments: Vec::new(),
            errors: Vec::new(),
            string_stack: Vec::new(),
        }
    }

    /// Main dispatch loop: consumes characters and emits tokens until EOF.
    fn run(&mut self) {
        while !self.at_end() {
            self.skip_whitespace();
            if self.at_end() {
                break;
            }

            let start = self.position();

            match self.peek() {
                '(' => self.single(TokenKind::LParen),
                ')' => self.single(TokenKind::RParen),
                '{' => {
                    self.single(TokenKind::LBrace);
                    if let Some(state) = self.string_stack.last_mut() {
                        state.brace_depth += 1;
                    }
                }
                '}' => {
                    if let Some(state) = self.string_stack.last_mut() {
                        if state.brace_depth > 0 {
                            state.brace_depth -= 1;
                            self.single(TokenKind::RBrace);
                        } else {
                            let mode = state.mode;
                            self.string_stack.pop();
                            let interp_end = self.position();
                            self.advance();
                            self.emit(TokenKind::InterpolEnd, interp_end);
                            self.lex_string_body(mode == StringMode::Multiline);
                        }
                    } else {
                        self.single(TokenKind::RBrace);
                    }
                }
                '[' => self.single(TokenKind::LBracket),
                ']' => self.single(TokenKind::RBracket),
                ',' => self.single(TokenKind::Comma),
                '@' => self.single(TokenKind::At),
                '%' => self.single(TokenKind::Percent),
                '.' => self.single(TokenKind::Dot),
                '?' => self.single(TokenKind::Question),
                '+' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::PlusEq),
                    _ => self.single(TokenKind::Plus),
                },
                '-' => match self.peek_next() {
                    Some('>') => self.double(TokenKind::Arrow),
                    Some('=') => self.double(TokenKind::MinusEq),
                    _ => self.single(TokenKind::Minus),
                },
                '*' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::StarEq),
                    _ => self.single(TokenKind::Star),
                },
                '/' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::SlashEq),
                    _ => self.single(TokenKind::Slash),
                },
                '=' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::EqEq),
                    _ => self.single(TokenKind::Eq),
                },
                '!' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::NotEq),
                    _ => {
                        self.errors.push(Diagnostic {
                            severity: Severity::Error,
                            message: "unexpected character '!'".into(),
                            hint: Some("use '!=' for not-equal comparison".into()),
                            span: Span::new(start, start),
                        });
                        self.advance();
                    }
                },
                '<' => match self.peek_next() {
                    Some('<') => self.double(TokenKind::LtLt),
                    Some('>') => self.double(TokenKind::LtGt),
                    Some('=') => self.double(TokenKind::LtEq),
                    _ => self.single(TokenKind::Lt),
                },
                '>' => match self.peek_next() {
                    Some('>') => self.double(TokenKind::GtGt),
                    Some('=') => self.double(TokenKind::GtEq),
                    _ => self.single(TokenKind::Gt),
                },
                '|' => self.single(TokenKind::Pipe),
                ':' => match self.peek_next() {
                    Some(':') => self.double(TokenKind::ColonColon),
                    _ => self.single(TokenKind::Colon),
                },
                'a'..='z' | '_' | 'A'..='Z' => self.lex_ident(),
                '0'..='9' => self.lex_number(),
                '#' => self.lex_comment(),
                '"' => {
                    if self.peek_next() == Some('"')
                        && self.chars.get(self.pos + 2).copied() == Some('"')
                    {
                        self.lex_multiline_string();
                    } else {
                        self.lex_string();
                    }
                }
                '\n' => self.lex_newline(),

                c => {
                    let end = self.position();
                    self.errors.push(Diagnostic {
                        severity: Severity::Error,
                        message: format!("unexpected character: '{c}'"),
                        hint: None,
                        span: Span::new(start, end),
                    });
                    self.advance();
                }
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.position(), self.position()),
        });
    }

    /// Scans an identifier or keyword. Uppercase-starting names become
    /// `TypeIdent`, lowercase names are checked against the keyword table.
    fn lex_ident(&mut self) {
        let start = self.position();
        let start_pos = self.pos;

        while !self.at_end() && self.is_ident_char(self.peek()) {
            self.advance();
        }

        let name: String = self.chars[start_pos..self.pos].iter().collect();
        let kind = self.keyword_or_ident(name);
        self.emit(kind, start);
    }

    fn is_ident_char(&self, c: char) -> bool {
        c.is_ascii_lowercase()
            || c.is_ascii_uppercase()
            || c.is_ascii_digit()
            || c == '_'
            || c == '?'
    }

    /// Maps a scanned name to a keyword token or an identifier/type token.
    fn keyword_or_ident(&self, name: String) -> TokenKind {
        if name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return TokenKind::TypeIdent(name);
        }
        match name.as_str() {
            "after" => TokenKind::After,
            "and" => TokenKind::And,
            "arena" => TokenKind::Arena,
            "break" => TokenKind::Break,
            "cond" => TokenKind::Cond,
            "const" => TokenKind::Const,
            "else" => TokenKind::Else,
            "end" => TokenKind::End,
            "enum" => TokenKind::Enum,
            "false" => TokenKind::False,
            "fn" => TokenKind::Fn,
            "for" => TokenKind::For,
            "if" => TokenKind::If,
            "impl" => TokenKind::Impl,
            "import" => TokenKind::Import,
            "in" => TokenKind::In,
            "loop" => TokenKind::Loop,
            "match" => TokenKind::Match,
            "move" => TokenKind::Move,
            "not" => TokenKind::Not,
            "or" => TokenKind::Or,
            "priv" => TokenKind::Priv,
            "protocol" => TokenKind::Protocol,
            "receive" => TokenKind::Receive,
            "return" => TokenKind::Return,
            "self" => TokenKind::Self_,
            "shared" => TokenKind::Shared,
            "spawn" => TokenKind::Spawn,
            "struct" => TokenKind::Struct,
            "true" => TokenKind::True,
            "type" => TokenKind::Type,
            "unless" => TokenKind::Unless,
            "when" => TokenKind::When,
            "while" => TokenKind::While,
            _ => TokenKind::Ident(name),
        }
    }

    /// Scans a numeric literal: decimal, hex (`0x`), binary (`0b`), or float.
    fn lex_number(&mut self) {
        let start = self.position();
        let start_pos = self.pos;

        if self.peek() == '0'
            && let Some(next) = self.peek_next()
        {
            if next == 'x' || next == 'X' {
                self.lex_prefixed_int(
                    start,
                    start_pos,
                    |c| c.is_ascii_hexdigit() || c == '_',
                    "expected hex digits after '0x'",
                    "hex literals use digits 0-9 and a-f, e.g. 0xFF",
                );
                return;
            } else if next == 'b' || next == 'B' {
                self.lex_prefixed_int(
                    start,
                    start_pos,
                    |c| c == '0' || c == '1' || c == '_',
                    "expected binary digits after '0b'",
                    "binary literals use digits 0 and 1, e.g. 0b1010",
                );
                return;
            }
        }

        while !self.at_end() && self.is_number_char(self.peek()) {
            self.advance();
        }

        if !self.at_end()
            && self.peek() == '.'
            && self.peek_next().is_some_and(|c| c.is_ascii_digit())
        {
            self.advance();
            while !self.at_end() && self.is_number_char(self.peek()) {
                self.advance();
            }
            let name: String = self.chars[start_pos..self.pos].iter().collect();
            self.emit(TokenKind::FloatLit(name), start);
            return;
        }

        let name: String = self.chars[start_pos..self.pos].iter().collect();
        self.emit(TokenKind::IntLit(name), start);
    }

    /// Advances past a two-char prefix (e.g. `0x`), scans digits matching
    /// `pred`, and emits an `IntLit` token or an error if no digits follow.
    fn lex_prefixed_int(
        &mut self,
        start: Position,
        start_pos: usize,
        pred: fn(char) -> bool,
        label: &str,
        hint: &str,
    ) {
        self.advance(); // 0
        self.advance(); // prefix char
        let digit_start = self.pos;
        while !self.at_end() && pred(self.peek()) {
            self.advance();
        }
        if self.pos == digit_start {
            self.errors.push(Diagnostic {
                severity: Severity::Error,
                message: label.into(),
                hint: Some(hint.into()),
                span: Span::new(start, self.position()),
            });
            return;
        }
        let name: String = self.chars[start_pos..self.pos].iter().collect();
        self.emit(TokenKind::IntLit(name), start);
    }

    fn is_number_char(&self, c: char) -> bool {
        c.is_ascii_digit() || c == '_'
    }

    /// Scans a `#`-prefixed comment and pushes it onto the comments list.
    fn lex_comment(&mut self) {
        let start = self.position();

        // Skip the leading '#'
        self.advance();
        // Skip optional space after '#'
        if !self.at_end() && self.peek() == ' ' {
            self.advance();
        }

        let text_start = self.pos;
        while !self.at_end() && self.peek() != '\n' {
            self.advance();
        }

        let text: String = self.chars[text_start..self.pos].iter().collect();
        self.comments.push(Comment {
            text,
            span: Span::new(start, self.position()),
        });
    }

    /// Emits a `Newline` token unless the newline should be suppressed
    /// (line continuation, leading dot, or duplicate newline).
    fn lex_newline(&mut self) {
        let start = self.position();
        self.advance();

        if self.continues_line() {
            return;
        }

        // Suppress newline before tokens that continue the previous expression
        if self.next_nonws_continues() {
            return;
        }

        // Collapse consecutive newlines into one
        if self.last_token_kind() == Some(&TokenKind::Newline) {
            return;
        }

        self.emit(TokenKind::Newline, start);
    }

    /// Peek past whitespace (and comment lines) to check if the next
    /// meaningful token starts with a continuation (`.`).
    fn next_nonws_continues(&self) -> bool {
        let mut i = self.pos;
        loop {
            // Skip whitespace
            while i < self.chars.len() && matches!(self.chars[i], ' ' | '\t' | '\r') {
                i += 1;
            }
            if i >= self.chars.len() {
                return false;
            }
            // Skip comment lines (# ... \n) and blank lines
            if self.chars[i] == '#' {
                while i < self.chars.len() && self.chars[i] != '\n' {
                    i += 1;
                }
                if i < self.chars.len() {
                    i += 1; // skip the \n
                }
                continue;
            }
            if self.chars[i] == '\n' {
                i += 1;
                continue;
            }
            break;
        }
        if i >= self.chars.len() {
            return false;
        }
        self.chars[i] == '.'
    }

    /// Returns true if the last token indicates the expression continues on
    /// the next line. Newlines after these tokens are suppressed.
    fn continues_line(&self) -> bool {
        match self.last_token_kind() {
            None => true,
            Some(kind) => matches!(
                kind,
                // Binary operators
                TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent
                    | TokenKind::And
                    | TokenKind::Or
                    | TokenKind::Not
                    | TokenKind::EqEq
                    | TokenKind::NotEq
                    | TokenKind::Lt
                    | TokenKind::Gt
                    | TokenKind::LtEq
                    | TokenKind::GtEq
                    // Assignment operators
                    | TokenKind::Eq
                    | TokenKind::PlusEq
                    | TokenKind::MinusEq
                    | TokenKind::StarEq
                    | TokenKind::SlashEq
                    // Punctuation that expects more
                    | TokenKind::Arrow
                    | TokenKind::Pipe
                    | TokenKind::Comma
                    | TokenKind::Dot
                    | TokenKind::Colon
                    | TokenKind::ColonColon
                    | TokenKind::LtGt
                    | TokenKind::At
                    // Opening delimiters
                    | TokenKind::LParen
                    | TokenKind::LBrace
                    | TokenKind::LBracket
                    | TokenKind::LtLt
                    // Keywords that start blocks
                    | TokenKind::Import
                    | TokenKind::Newline
            ),
        }
    }

    /// Opens a single-line string (`"`) and enters the string body scanner.
    fn lex_string(&mut self) {
        let start = self.position();
        self.advance(); // opening "
        self.emit(TokenKind::StringStart, start);
        self.lex_string_body(false);
    }

    /// Opens a triple-quoted multiline string (`"""`) and enters the string body scanner.
    fn lex_multiline_string(&mut self) {
        let start = self.position();
        self.advance(); // "
        self.advance(); // "
        self.advance(); // "
        self.emit(TokenKind::MultilineStringStart, start);
        self.lex_string_body(true);
    }

    /// Scans the interior of a string literal, emitting `StringFragment`,
    /// `InterpolStart`/`InterpolEnd`, escape sequences, and the closing delimiter.
    fn lex_string_body(&mut self, multiline: bool) {
        let frag_start = self.position();
        let mut text = String::new();

        loop {
            if self.at_end() {
                self.emit_fragment(&mut text, frag_start);
                let (label, hint) = if multiline {
                    ("unterminated multiline string", "add a closing '\"\"\"'")
                } else {
                    ("unterminated string", "add a closing '\"'")
                };
                self.errors.push(Diagnostic {
                    severity: Severity::Error,
                    message: label.into(),
                    hint: Some(hint.into()),
                    span: Span::new(frag_start, self.position()),
                });
                return;
            }

            let c = self.peek();

            if !multiline && c == '"' {
                self.emit_fragment(&mut text, frag_start);
                let end_start = self.position();
                self.advance();
                self.emit(TokenKind::StringEnd, end_start);
                return;
            }

            if multiline
                && c == '"'
                && self.peek_next() == Some('"')
                && self.chars.get(self.pos + 2).copied() == Some('"')
            {
                self.emit_fragment(&mut text, frag_start);
                let end_start = self.position();
                self.advance();
                self.advance();
                self.advance();
                self.emit(TokenKind::MultilineStringEnd, end_start);
                return;
            }

            if !multiline && c == '\n' {
                self.emit_fragment(&mut text, frag_start);
                self.errors.push(Diagnostic {
                    severity: Severity::Error,
                    message: "unterminated string".into(),
                    hint: Some("add a closing '\"'".into()),
                    span: Span::new(frag_start, self.position()),
                });
                return;
            }

            if c == '#' && self.peek_next() == Some('{') {
                self.emit_fragment(&mut text, frag_start);
                let interp_start = self.position();
                self.advance();
                self.advance();
                self.emit(TokenKind::InterpolStart, interp_start);
                let mode = if multiline {
                    StringMode::Multiline
                } else {
                    StringMode::Single
                };
                self.string_stack.push(InterpolState {
                    mode,
                    brace_depth: 0,
                });
                return;
            }

            if c == '\\'
                && let Some(next) = self.peek_next()
            {
                let mapped = match next {
                    '"' => Some('"'),
                    '\\' => Some('\\'),
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    '#' => Some('#'),
                    _ => None,
                };
                if let Some(ch) = mapped {
                    self.advance();
                    self.advance();
                    text.push(ch);
                } else {
                    let esc_start = self.position();
                    self.advance();
                    self.advance();
                    self.errors.push(Diagnostic {
                        severity: Severity::Error,
                        message: format!("unknown escape sequence '\\{next}'"),
                        hint: Some("supported escapes: \\\\, \\\", \\n, \\r, \\t, \\#".into()),
                        span: Span::new(esc_start, self.position()),
                    });
                    text.push('\\');
                    text.push(next);
                }
                continue;
            }

            self.advance();
            text.push(c);
        }
    }

    /// Emits a `StringFragment` token if `text` is non-empty, draining it.
    fn emit_fragment(&mut self, text: &mut String, frag_start: Position) {
        if !text.is_empty() {
            self.emit(TokenKind::StringFragment(std::mem::take(text)), frag_start);
        }
    }

    /// Current char without advancing. Panics if at end (always check at_end first).
    fn peek(&self) -> char {
        self.chars[self.pos]
    }

    /// Next char (one ahead of current), or None if at/near end.
    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    /// Consume current char, advance position, update line/column.
    fn advance(&mut self) -> char {
        let c = self.chars[self.pos];
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        c
    }

    /// Are we past the end of input?
    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// Current source position (for building spans).
    fn position(&self) -> Position {
        Position {
            offset: self.pos as u32,
            line: self.line,
            column: self.column,
        }
    }

    /// Skip spaces and tabs (NOT newlines -- those are significant).
    fn skip_whitespace(&mut self) {
        while !self.at_end() && matches!(self.peek(), ' ' | '\t' | '\r') {
            self.advance();
        }
    }

    /// Emit a single-char token and advance.
    fn single(&mut self, kind: TokenKind) {
        let start = self.position();
        self.advance();
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.position()),
        });
    }

    /// Emit a double-char token and advance.
    fn double(&mut self, kind: TokenKind) {
        let start = self.position();
        self.advance();
        self.advance();
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.position()),
        });
    }

    /// Emit a token with an explicit start position (for multi-char tokens).
    fn emit(&mut self, kind: TokenKind, start: Position) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.position()),
        });
    }

    /// The last significant token kind emitted, if any.
    fn last_token_kind(&self) -> Option<&TokenKind> {
        self.tokens.last().map(|t| &t.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_kinds(source: &str) -> Vec<TokenKind> {
        let result = lex(source);
        result.tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_empty_source() {
        assert_eq!(lex_kinds(""), vec![TokenKind::Eof]);
    }

    #[test]
    fn test_whitespace_only() {
        assert_eq!(lex_kinds("   \t  "), vec![TokenKind::Eof]);
    }

    #[test]
    fn test_simple_string() {
        assert_eq!(
            lex_kinds(r#""hello""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("hello".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_escape_newline() {
        assert_eq!(
            lex_kinds(r#""hello\nworld""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("hello\nworld".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_escape_tab() {
        assert_eq!(
            lex_kinds(r#""col1\tcol2""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("col1\tcol2".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_escape_quote() {
        assert_eq!(
            lex_kinds(r#""say \"hello\"""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("say \"hello\"".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_escape_backslash() {
        assert_eq!(
            lex_kinds(r#""path\\file""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("path\\file".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_escape_hash() {
        assert_eq!(
            lex_kinds(r#""use \#{name}""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("use #{name}".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_unknown_escape() {
        let result = lex(r#""bad\x""#);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].message.contains("unknown escape"));
    }

    #[test]
    fn test_interpolation_basic() {
        assert_eq!(
            lex_kinds(r##""hello #{name}""##),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("hello ".into()),
                TokenKind::InterpolStart,
                TokenKind::Ident("name".into()),
                TokenKind::InterpolEnd,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_interpolation_at_start() {
        assert_eq!(
            lex_kinds(r##""#{x} done""##),
            vec![
                TokenKind::StringStart,
                TokenKind::InterpolStart,
                TokenKind::Ident("x".into()),
                TokenKind::InterpolEnd,
                TokenKind::StringFragment(" done".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_interpolation_multiple() {
        assert_eq!(
            lex_kinds(r##""#{a} and #{b}""##),
            vec![
                TokenKind::StringStart,
                TokenKind::InterpolStart,
                TokenKind::Ident("a".into()),
                TokenKind::InterpolEnd,
                TokenKind::StringFragment(" and ".into()),
                TokenKind::InterpolStart,
                TokenKind::Ident("b".into()),
                TokenKind::InterpolEnd,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_interpolation_nested_braces() {
        assert_eq!(
            lex_kinds(r##""#{map{key: 1}}""##),
            vec![
                TokenKind::StringStart,
                TokenKind::InterpolStart,
                TokenKind::Ident("map".into()),
                TokenKind::LBrace,
                TokenKind::Ident("key".into()),
                TokenKind::Colon,
                TokenKind::IntLit("1".into()),
                TokenKind::RBrace,
                TokenKind::InterpolEnd,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_multiline_escapes() {
        let src = r##""""hello\nworld""""##;
        assert_eq!(
            lex_kinds(src),
            vec![
                TokenKind::MultilineStringStart,
                TokenKind::StringFragment("hello\nworld".into()),
                TokenKind::MultilineStringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_multiline_interpolation() {
        let src = r##""""hello #{name}""""##;
        assert_eq!(
            lex_kinds(src),
            vec![
                TokenKind::MultilineStringStart,
                TokenKind::StringFragment("hello ".into()),
                TokenKind::InterpolStart,
                TokenKind::Ident("name".into()),
                TokenKind::InterpolEnd,
                TokenKind::MultilineStringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(
            lex_kinds(r#""""#),
            vec![TokenKind::StringStart, TokenKind::StringEnd, TokenKind::Eof,]
        );
    }

    #[test]
    fn test_hash_without_brace() {
        assert_eq!(
            lex_kinds(r#""color #fff""#),
            vec![
                TokenKind::StringStart,
                TokenKind::StringFragment("color #fff".into()),
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }
}
