mod common;

use common::*;
use koja_ast::ast::{ExprKind, Statement, StringPart};
use koja_ast::util::dedent;
use koja_fmt::{FormatResult, format};
use koja_parser::ParseMode;

/// Cooked value of the first string expression in dedented script
/// `source`, with interpolations rendered as `#{...}` placeholders.
fn first_string_value(source: &str) -> String {
    let result = koja_parser::parse(&dedent(source), ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "parse errors: {:?}",
        result.errors
    );
    let expr = result
        .ast
        .body
        .unwrap_or_default()
        .into_iter()
        .find_map(|stmt| match stmt {
            Statement::Assignment { value, .. } | Statement::Expr(value) => Some(value),
            _ => None,
        })
        .expect("no expression in script");
    match expr.kind {
        ExprKind::String { parts, .. } => parts
            .iter()
            .map(|p| match p {
                StringPart::Literal { value, .. } => value.clone(),
                StringPart::Interpolation { .. } => "#{...}".to_string(),
            })
            .collect(),
        other => panic!("expected string expression, got {other:?}"),
    }
}

#[test]
fn heredoc_broken_opener_round_trips() {
    assert_unchanged(
        r#"
            fn scaffold -> String
              content =
                """
                line one
                  nested deeper
                line two
                """
              content
            end
        "#,
    );
}

#[test]
fn heredoc_glued_opener_round_trips() {
    assert_unchanged(
        r#"
            fn scaffold -> String
              content = """
              line one
              line two
              """
              content
            end
        "#,
    );
}

#[test]
fn heredoc_glued_normalizes_content_to_ambient_indent() {
    let input = r#"
            x = """
              hello
                nested
              """
        "#;
    let output = fmt_script(input);
    assert_formatted(
        output.clone(),
        r#"
            x = """
            hello
              nested
            """
        "#,
    );
    // The cooked value survives the re-indent byte-exactly.
    assert_eq!(first_string_value(input), "hello\n  nested");
    assert_eq!(first_string_value(&output), "hello\n  nested");
}

#[test]
fn heredoc_blank_lines_and_trailing_newline_round_trip() {
    let source = r#"
            x = """
            hello

            world

            """
        "#;
    assert_fmt_script(source, source);
    assert_eq!(first_string_value(source), "hello\n\nworld\n");
}

#[test]
fn heredoc_interpolation_round_trips() {
    assert_unchanged(
        r#"
            fn greet(name: String) -> String
              """
              hello #{name}
              bye
              """
            end
        "#,
    );
}

#[test]
fn heredoc_quotes_round_trip() {
    // Lone quotes stay raw, even at line end. A would-be closing
    // run keeps its escaped third quote.
    assert_unchanged(
        r#"
            fn f -> String
              """
              say "hi"
              a quoted run: ""\"
              """
            end
        "#,
    );
}

#[test]
fn heredoc_sole_call_argument_hugs() {
    assert_unchanged(
        r#"
            fn f
              IO.puts("""
              hello
              """)
            end
        "#,
    );

    // An exploded sole-heredoc argument list collapses to the hug.
    assert_fmt(
        r#"
            fn f
              IO.puts(
                """
                hello
                """,
              )
            end
        "#,
        r#"
            fn f
              IO.puts("""
              hello
              """)
            end
        "#,
    );
}

#[test]
fn heredoc_trailing_space_falls_back_to_single_line() {
    // The renderer trims line ends, so trailing spaces cannot survive
    // heredoc form. Both mid-content and content-final trailing
    // spaces round-trip through the single-line spelling.
    let mid = "x = \"\"\"\nkeep \ntrailing\n\"\"\"\n";
    match format(mid, ParseMode::Script) {
        FormatResult::Ok(out) => assert_eq!(out, "x = \"keep \\ntrailing\"\n"),
        FormatResult::ParseErrors(e) => panic!("parse error: {e:?}"),
    }

    let last = "x = \"\"\"\nkeep \n\"\"\"\n";
    match format(last, ParseMode::Script) {
        FormatResult::Ok(out) => assert_eq!(out, "x = \"keep \"\n"),
        FormatResult::ParseErrors(e) => panic!("parse error: {e:?}"),
    }
}
