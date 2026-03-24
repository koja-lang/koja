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

/// Formats a `project.expo` file (a bare expression, not a full module).
///
/// Wraps the source in a dummy function so the parser can handle it,
/// formats the wrapper, then extracts and un-indents the body.
pub fn format_project(source: &str) -> FormatResult {
    format_project_width(source, 80)
}

/// Formats a `project.expo` file, wrapping lines at `width` columns.
pub fn format_project_width(source: &str, width: u32) -> FormatResult {
    let wrapped = format!("fn __project__\n{source}\nend");
    match format_width(&wrapped, width) {
        FormatResult::Ok(formatted) => {
            let body = formatted
                .strip_prefix("fn __project__\n")
                .and_then(|s| s.strip_suffix("\nend\n"))
                .unwrap_or(&formatted);

            let mut out: String = body
                .lines()
                .map(|line| line.strip_prefix("  ").unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n");

            if !out.ends_with('\n') {
                out.push('\n');
            }
            FormatResult::Ok(out)
        }
        err => err,
    }
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

    #[test]
    fn cond_chain_with_not() {
        assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999 or not x == y -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999 or
                not x == y ->
                  true

                else ->
                  false
              end
            end
        ");
    }

    #[test]
    fn cond_and_chain_packs_like_fill() {
        assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and y != 50 and x != 99 -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and
                y != 50 and x != 99 ->
                  true

                else ->
                  false
              end
            end
        ");
    }

    #[test]
    fn short_closure_inline() {
        assert_fmt(
            "
            fn apply(f: fn(Int) -> Int, x: Int) -> Int
              f(x)
            end

            fn main
              apply(x -> x * 2, 5)
            end
        ",
            "
            fn apply(f: fn(Int) -> Int, x: Int) -> Int
              f(x)
            end

            fn main
              apply(x -> x * 2, 5)
            end
        ",
        );
    }

    #[test]
    fn block_closure_formatting() {
        assert_fmt(
            "
            fn main
              f =
                fn (x: Int, y: Int) -> Int x + y end
            end
        ",
            "
            fn main
              f =
                fn (x: Int, y: Int) -> Int x + y end
            end
        ",
        );
    }

    #[test]
    fn binary_literal_formatting() {
        assert_fmt(
            "
            fn main
              b = <<1, 2, 3>>
              c = <<header::8, payload::16 big>>
            end
        ",
            "
            fn main
              b = <<1, 2, 3>>
              c = <<header::8, payload::16 big>>
            end
        ",
        );
    }

    #[test]
    fn concat_operator() {
        assert_fmt(
            r#"
            fn main
              s = "hello" <> " " <> "world"
            end
        "#,
            r#"
            fn main
              s = "hello" <> " " <> "world"
            end
        "#,
        );
    }

    #[test]
    fn struct_construction_short_inline() {
        assert_fmt(
            r#"
            fn main
              c = Config{name: "yo", enabled: true}
            end
        "#,
            r#"
            fn main
              c = Config{name: "yo", enabled: true}
            end
        "#,
        );
    }

    #[test]
    fn struct_construction_long_multiline() {
        assert_fmt(
            r#"
            fn main
              c = Config{name: "a very long name here", enabled: true, verbose: false, timeout: 3000}
            end
        "#,
            r#"
            fn main
              c = Config{
                name: "a very long name here",
                enabled: true,
                verbose: false,
                timeout: 3000,
              }
            end
        "#,
        );
    }

    #[test]
    fn ternary_expression() {
        assert_fmt(
            "
            fn f(x: Int) -> Int
              y = x > 0 ? x : -x
              y
            end
        ",
            "
            fn f(x: Int) -> Int
              y = x > 0 ? x : -x
              y
            end
        ",
        );
    }

    #[test]
    fn doc_on_type_alias() {
        assert_fmt(
            "
            @doc \"A user ID.\"
            type UserId = Int
        ",
            "
            @doc \"A user ID.\"
            type UserId = Int
        ",
        );
    }

    #[test]
    fn cond_or_chain_packs_like_fill() {
        assert_fmt(
            r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta" or x == "echo" or x == "foxtrot" or x == "golf" -> "nato"
                else -> "other"
              end
            end
        "#,
            r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta" or
                x == "echo" or x == "foxtrot" or x == "golf" ->
                  "nato"

                else ->
                  "other"
              end
            end
        "#,
        );
    }
}
