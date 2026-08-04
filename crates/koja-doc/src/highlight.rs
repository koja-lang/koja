//! Minimal Koja syntax highlighter for doc pages. Token classes
//! (`kw` / `ty` / `fn` / `st` / `nm` / `cm` / `at`) mirror the
//! website's Rouge lexer so generated docs and kojalang.org code
//! blocks read identically.

use crate::extract::DocFunction;

const KEYWORDS: &[&str] = &[
    "after", "alias", "and", "as", "break", "cond", "const", "else", "end", "enum", "extend",
    "fail", "false", "fn", "for", "if", "impl", "in", "loop", "match", "not", "or", "priv",
    "protocol", "receive", "rescue", "return", "self", "spawn", "struct", "true", "try", "type",
    "unless", "when", "while",
];

/// Highlight a Koja code block. Returns inner HTML for a `<code>`
/// element with all source text HTML-escaped.
pub fn highlight_koja(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len() * 2);
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'#' => i = eat_comment(code, i, &mut out),
            b'"' => i = eat_string(code, i, &mut out),
            b'@' => i = eat_annotation(code, i, &mut out),
            b'0'..=b'9' => i = eat_number(code, i, &mut out),
            c if c == b'_' || c.is_ascii_alphabetic() => i = eat_ident(code, i, &mut out),
            _ => {
                push_escaped(&mut out, &code[i..i + 1]);
                i += 1;
            }
        }
    }
    out
}

/// One styled run in a rendered signature. `Plain` runs carry no
/// highlight class.
enum SignatureToken {
    Function,
    Keyword,
    Plain,
    Type,
}

impl SignatureToken {
    fn class(&self) -> Option<&'static str> {
        match self {
            SignatureToken::Function => Some("fn"),
            SignatureToken::Keyword => Some("kw"),
            SignatureToken::Plain => None,
            SignatureToken::Type => Some("ty"),
        }
    }
}

impl DocFunction {
    /// Break this signature into styled runs, the shared source for
    /// both the HTML and plain-text renderings.
    fn signature_segments(&self) -> Vec<(SignatureToken, String)> {
        use SignatureToken::{Function, Keyword, Plain, Type};

        let mut segments = vec![
            (Keyword, "fn".to_string()),
            (Plain, " ".to_string()),
            (Function, self.name.clone()),
        ];

        if !self.type_params.is_empty() {
            segments.push((Plain, "<".to_string()));
            segments.push((Type, self.type_params.join(", ")));
            segments.push((Plain, ">".to_string()));
        }

        segments.push((Plain, "(".to_string()));
        for (idx, p) in self.params.iter().enumerate() {
            if idx > 0 {
                segments.push((Plain, ", ".to_string()));
            }
            if p.name == "self" {
                segments.push((Keyword, "self".to_string()));
                continue;
            }
            segments.push((Plain, p.name.clone()));
            if !p.type_name.is_empty() {
                segments.push((Plain, ": ".to_string()));
                segments.push((Type, p.type_name.clone()));
            }
        }
        segments.push((Plain, ")".to_string()));

        if let Some(ret) = &self.return_type {
            segments.push((Plain, " -> ".to_string()));
            segments.push((Type, ret.clone()));
        }
        if let Some(err) = &self.error_type {
            segments.push((Plain, " ! ".to_string()));
            segments.push((Type, err.clone()));
        }
        segments
    }

    /// Render this signature as highlighted HTML for the docs'
    /// code panels. Called from the `function_detail` template.
    pub fn signature_html(&self) -> String {
        let mut out = String::new();
        for (token, text) in self.signature_segments() {
            match token.class() {
                Some(class) => span(&mut out, class, &text),
                None => push_escaped(&mut out, &text),
            }
        }
        out
    }

    /// Render this signature as plain text for terminal output.
    pub fn signature_text(&self) -> String {
        self.signature_segments()
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }
}

fn span(out: &mut String, class: &str, text: &str) {
    out.push_str("<span class=\"");
    out.push_str(class);
    out.push_str("\">");
    push_escaped(out, text);
    out.push_str("</span>");
}

fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// `#` comment through end of line.
fn eat_comment(code: &str, start: usize, out: &mut String) -> usize {
    let end = code[start..]
        .find('\n')
        .map_or(code.len(), |off| start + off);
    span(out, "cm", &code[start..end]);
    end
}

/// String literal, `"..."` or `"""..."""`, interpolation kept inside.
fn eat_string(code: &str, start: usize, out: &mut String) -> usize {
    let bytes = code.as_bytes();
    let triple = code[start..].starts_with("\"\"\"");
    let quote_len = if triple { 3 } else { 1 };
    let mut i = start + quote_len;

    while i < bytes.len() {
        if bytes[i] == b'\\' && !triple {
            i += 2;
            continue;
        }
        if triple {
            if code[i..].starts_with("\"\"\"") {
                i += 3;
                break;
            }
        } else if bytes[i] == b'"' || bytes[i] == b'\n' {
            i += usize::from(bytes[i] == b'"');
            break;
        }
        i += 1;
    }
    let end = i.min(code.len());
    span(out, "st", &code[start..end]);
    end
}

/// `@doc`-style annotation marker.
fn eat_annotation(code: &str, start: usize, out: &mut String) -> usize {
    let end = ident_end(code, start + 1);
    if end == start + 1 {
        push_escaped(out, "@");
        return start + 1;
    }
    span(out, "at", &code[start..end]);
    end
}

/// Integer, float, hex, or binary literal with `_` separators.
fn eat_number(code: &str, start: usize, out: &mut String) -> usize {
    let bytes = code.as_bytes();
    let mut i = start;
    if code[start..].starts_with("0x") || code[start..].starts_with("0b") {
        i += 2;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
    } else {
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_' || bytes[i] == b'.')
        {
            // Stop at a `.` not followed by a digit (method call, range).
            if bytes[i] == b'.' && !bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                break;
            }
            i += 1;
        }
    }
    span(out, "nm", &code[start..i]);
    i
}

/// Identifier or keyword, including `?` / `!` name suffixes.
fn eat_ident(code: &str, start: usize, out: &mut String) -> usize {
    let bytes = code.as_bytes();
    let mut end = ident_end(code, start);
    let has_suffix = bytes.get(end).is_some_and(|&c| c == b'?');
    if has_suffix {
        end += 1;
    }
    let word = &code[start..end];

    if !has_suffix && KEYWORDS.contains(&word) {
        span(out, "kw", word);
    } else if bytes[start].is_ascii_uppercase() {
        span(out, "ty", word);
    } else if next_nonspace(code, end) == Some(b'(') {
        span(out, "fn", word);
    } else {
        push_escaped(out, word);
    }
    end
}

fn ident_end(code: &str, start: usize) -> usize {
    let bytes = code.as_bytes();
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i
}

fn next_nonspace(code: &str, from: usize) -> Option<u8> {
    code.as_bytes()[from..]
        .iter()
        .copied()
        .find(|c| !c.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::DocParam;

    #[test]
    fn highlights_keywords_types_and_strings() {
        let html = highlight_koja("fn greet(name: String)\n  \"hi #{name}\" # welcome\nend");
        assert!(html.contains("<span class=\"kw\">fn</span>"));
        assert!(html.contains("<span class=\"fn\">greet</span>"));
        assert!(html.contains("<span class=\"ty\">String</span>"));
        assert!(html.contains("<span class=\"st\">&quot;hi #{name}&quot;</span>"));
        assert!(html.contains("<span class=\"cm\"># welcome</span>"));
        assert!(html.contains("<span class=\"kw\">end</span>"));
    }

    #[test]
    fn boolean_suffix_names_are_not_keywords() {
        let html = highlight_koja("empty?(x) if in?");
        assert!(html.contains("<span class=\"fn\">empty?</span>"));
        assert!(html.contains("<span class=\"kw\">if</span>"));
        assert!(!html.contains("<span class=\"kw\">in?</span>"));
    }

    #[test]
    fn escapes_html_in_code() {
        let html = highlight_koja("a < b && c > d");
        assert!(html.contains("&lt;"));
        assert!(html.contains("&amp;&amp;"));
        assert!(html.contains("&gt;"));
    }

    fn checkout_function() -> DocFunction {
        DocFunction {
            doc: None,
            error_type: Some("PoolError".to_string()),
            name: "checkout".to_string(),
            params: vec![
                DocParam {
                    name: "self".to_string(),
                    type_name: String::new(),
                },
                DocParam {
                    name: "timeout".to_string(),
                    type_name: "Int32".to_string(),
                },
            ],
            return_type: Some("Conn".to_string()),
            type_params: vec![],
        }
    }

    #[test]
    fn signature_renders_fallible_generic_function() {
        let html = checkout_function().signature_html();
        assert_eq!(
            html,
            "<span class=\"kw\">fn</span> <span class=\"fn\">checkout</span>\
             (<span class=\"kw\">self</span>, timeout: <span class=\"ty\">Int32</span>) \
             -&gt; <span class=\"ty\">Conn</span> ! <span class=\"ty\">PoolError</span>"
        );
    }

    #[test]
    fn signature_text_renders_plain_form() {
        assert_eq!(
            checkout_function().signature_text(),
            "fn checkout(self, timeout: Int32) -> Conn ! PoolError"
        );
    }
}
