use expo_ast::ast::{Diagnostic, Severity};

use crate::{Comment, Position, Span, Token, TokenKind};

// =============================================================================
// Unfinished lexer features (not needed for basic auth-manager-expo parsing):
//
// - String interpolation: #{expr} and #{expr:format_spec} inside strings.
//   Requires a mode stack to switch between string and normal lexing.
//   Token sequence: StringStart, StringFragment, InterpolStart, ...tokens...,
//   InterpolEnd, StringFragment, StringEnd.
//
// - Multiline strings: """...""" delimiters. Similar to regular strings but
//   allow newlines. Need to detect three consecutive " chars.
//
// - Escape sequences in strings: \", \\, \n, \t, etc. Currently the lexer
//   treats backslash as a regular character inside strings.
//
// - Hex integer literals: 0x1F, 0xFF_00. Check for '0x' prefix then consume
//   hex digits [0-9a-fA-F_].
//
// - Binary integer literals: 0b1010, 0b1111_0000. Check for '0b' prefix then
//   consume [01_].
// =============================================================================

#[derive(Debug)]
pub struct LexResult {
    pub comments: Vec<Comment>,
    pub errors: Vec<Diagnostic>,
    pub tokens: Vec<Token>,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    column: u32,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    errors: Vec<Diagnostic>,
}

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
        }
    }

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
                '{' => self.single(TokenKind::LBrace),
                '}' => self.single(TokenKind::RBrace),
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
                    Some('=') => self.double(TokenKind::LtEq),
                    _ => self.single(TokenKind::Lt),
                },
                '>' => match self.peek_next() {
                    Some('=') => self.double(TokenKind::GtEq),
                    _ => self.single(TokenKind::Gt),
                },
                '|' => match self.peek_next() {
                    Some('>') => self.double(TokenKind::PipeRight),
                    _ => {
                        self.errors.push(Diagnostic {
                            severity: Severity::Error,
                            message: "unexpected character '|'".into(),
                            hint: Some("use '|>' for the pipe operator".into()),
                            span: Span::new(start, start),
                        });
                        self.advance();
                    }
                },
                ':' => match self.peek_next() {
                    Some(':') => self.double(TokenKind::ColonColon),
                    _ => self.single(TokenKind::Colon),
                },
                'a'..='z' | '_' => self.lex_ident(),
                'A'..='Z' => self.lex_upper_ident(),
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

    fn keyword_or_ident(&self, name: String) -> TokenKind {
        match name.as_str() {
            "and" => TokenKind::And,
            "arena" => TokenKind::Arena,
            "await" => TokenKind::Await,
            "break" => TokenKind::Break,
            "cond" => TokenKind::Cond,
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
            "receive" => TokenKind::Receive,
            "ref" => TokenKind::Ref,
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
            "none" => TokenKind::None_,
            _ => TokenKind::Ident(name),
        }
    }

    fn lex_upper_ident(&mut self) {
        let start = self.position();
        let start_pos = self.pos;
        let mut is_const = true;

        while !self.at_end() {
            let c = self.peek();
            if !self.is_ident_char(c) {
                break;
            }

            is_const = is_const && self.is_upper_ident_char(c);
            self.advance();
        }

        let name: String = self.chars[start_pos..self.pos].iter().collect();
        let kind = if is_const {
            TokenKind::ConstIdent(name)
        } else {
            TokenKind::TypeIdent(name)
        };
        self.emit(kind, start);
    }

    fn is_upper_ident_char(&self, c: char) -> bool {
        c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
    }

    fn lex_number(&mut self) {
        let start = self.position();
        let start_pos = self.pos;

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

    fn is_number_char(&self, c: char) -> bool {
        c.is_ascii_digit() || c == '_'
    }

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
    /// meaningful token starts with a continuation (`.` or `|>`).
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
        match self.chars[i] {
            '.' => true,
            '|' => i + 1 < self.chars.len() && self.chars[i + 1] == '>',
            _ => false,
        }
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
                    | TokenKind::PipeRight
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
                    | TokenKind::Comma
                    | TokenKind::Dot
                    | TokenKind::Colon
                    | TokenKind::ColonColon
                    | TokenKind::At
                    // Opening delimiters
                    | TokenKind::LParen
                    | TokenKind::LBrace
                    | TokenKind::LBracket
                    // Keywords that start blocks
                    | TokenKind::Import
                    | TokenKind::Newline
            ),
        }
    }

    fn lex_string(&mut self) {
        let start = self.position();
        self.advance();
        self.emit(TokenKind::StringStart, start);

        let frag_start = self.position();
        let frag_start_pos = self.pos;

        while !self.at_end() && self.peek() != '"' && self.peek() != '\n' {
            self.advance();
        }

        if frag_start_pos < self.pos {
            let text: String = self.chars[frag_start_pos..self.pos].iter().collect();
            self.emit(TokenKind::StringFragment(text), frag_start);
        }

        if !self.at_end() && self.peek() == '"' {
            let end_start = self.position();
            self.advance();
            self.emit(TokenKind::StringEnd, end_start);
        } else {
            self.errors.push(Diagnostic {
                severity: Severity::Error,
                message: "unterminated string".into(),
                hint: Some("add a closing '\"'".into()),
                span: Span::new(start, self.position()),
            });
        }
    }

    fn lex_multiline_string(&mut self) {
        let start = self.position();
        self.advance(); // "
        self.advance(); // "
        self.advance(); // "
        self.emit(TokenKind::MultilineStringStart, start);

        let frag_start = self.position();
        let frag_start_pos = self.pos;

        while !self.at_end() {
            if self.peek() == '"'
                && self.peek_next() == Some('"')
                && self.chars.get(self.pos + 2).copied() == Some('"')
            {
                break;
            }
            self.advance();
        }

        if frag_start_pos < self.pos {
            let text: String = self.chars[frag_start_pos..self.pos].iter().collect();
            self.emit(TokenKind::StringFragment(text), frag_start);
        }

        if !self.at_end() {
            let end_start = self.position();
            self.advance(); // "
            self.advance(); // "
            self.advance(); // "
            self.emit(TokenKind::MultilineStringEnd, end_start);
        } else {
            self.errors.push(Diagnostic {
                severity: Severity::Error,
                message: "unterminated multiline string".into(),
                hint: Some("add a closing '\"\"\"'".into()),
                span: Span::new(start, self.position()),
            });
        }
    }

    // =====================================================================
    // Helpers -- these are yours to use
    // =====================================================================

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
}
