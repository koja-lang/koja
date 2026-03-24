pub mod doc;
pub mod printer;

use doc::render;
use expo_ast::ast::Diagnostic;

/// The result of formatting a source string.
pub enum FormatResult {
    /// Successfully formatted source code.
    Ok(String),
    /// The source could not be parsed; contains the parse diagnostics.
    ParseErrors(Vec<Diagnostic>),
}

/// Formats Expo source code using the default line width (80 columns).
pub fn format(source: &str) -> FormatResult {
    format_width(source, 80)
}

/// Formats Expo source code, wrapping lines at `width` columns.
pub fn format_width(source: &str, width: u32) -> FormatResult {
    let result = expo_parser::parse(source);
    if !result.errors.is_empty() {
        return FormatResult::ParseErrors(result.errors);
    }

    let doc = printer::module_to_doc(&result.module);
    let rendered = render(&doc, width);
    let mut out: String = rendered
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");

    if !out.ends_with('\n') {
        out.push('\n');
    }

    FormatResult::Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        match format(&dedent(source)) {
            FormatResult::Ok(s) => s,
            FormatResult::ParseErrors(e) => panic!("parse error: {:?}", e),
        }
    }

    fn assert_fmt(input: &str, expected: &str) {
        let actual = fmt(input);
        let mut expected = dedent(expected);
        if !expected.ends_with('\n') {
            expected.push('\n');
        }
        assert_eq!(
            actual, expected,
            "\n--- actual ---\n{actual}--- expected ---\n{expected}"
        );
    }

    fn dedent(s: &str) -> String {
        let s = s.strip_prefix('\n').unwrap_or(s);
        let min_indent = s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        s.lines()
            .map(|l| {
                if l.len() >= min_indent {
                    &l[min_indent..]
                } else {
                    l.trim()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn or_pattern_short_stays_inline() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
            "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
        );
    }

    #[test]
    fn or_pattern_long_wraps_with_trailing_pipe() {
        assert_fmt(
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                _ -> false
              end
            end
        "#,
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
        );
    }

    #[test]
    fn or_pattern_multiline_arms_have_blank_lines() {
        assert_fmt(
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" | "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" | "Y" | "Z" -> true
                _ -> false
              end
            end
        "#,
            r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" |
                "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" |
                "Y" | "Z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
        );
    }
}
