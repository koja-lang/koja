//! String-literal construction: triple-quoted multiline indentation,
//! interpolation, and escape handling all the way through the parser
//! into the AST. The dedent contract (closing-quote column is the
//! indent oracle, leading newline stripped, trailing newline trimmed)
//! lives in `construct/string.rs` and is exercised here end-to-end.

use koja_ast::ast::{ExprKind, StringPart};

mod common;

use common::first_script_expr;

fn parse_string_parts(source: &str) -> Vec<StringPart> {
    match first_script_expr(source).kind {
        ExprKind::String { parts, .. } => parts,
        other => panic!("expected string expression, got {other:?}"),
    }
}

fn literal_values(parts: &[StringPart]) -> Vec<&str> {
    parts
        .iter()
        .filter_map(|p| match p {
            StringPart::Literal { value, .. } => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn test_dedent_basic() {
    let src = "x = \"\"\"\n  hello\n  world\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(literal_values(&parts), vec!["hello\nworld"]);
}

#[test]
fn test_dedent_preserves_extra_indent() {
    let src = "x = \"\"\"\n  line1\n    indented\n  line3\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(literal_values(&parts), vec!["line1\n  indented\nline3"]);
}

#[test]
fn test_dedent_empty_lines_preserved() {
    let src = "x = \"\"\"\n  hello\n\n  world\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(literal_values(&parts), vec!["hello\n\nworld"]);
}

#[test]
fn test_dedent_trailing_newline_stripped() {
    let src = "x = \"\"\"\n  hello\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    let vals = literal_values(&parts);
    assert_eq!(vals, vec!["hello"]);
    assert!(!vals[0].ends_with('\n'));
}

#[test]
fn test_dedent_with_interpolation() {
    let src = "x = \"\"\"\n  hello #{name}\n  world\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(parts.len(), 3);
    match &parts[0] {
        StringPart::Literal { value, .. } => assert_eq!(value, "hello "),
        _ => panic!("expected literal"),
    }
    assert!(matches!(&parts[1], StringPart::Interpolation { .. }));
    match &parts[2] {
        StringPart::Literal { value, .. } => assert_eq!(value, "\nworld"),
        _ => panic!("expected literal"),
    }
}

#[test]
fn test_dedent_no_indent() {
    let src = "x = \"\"\"\nhello\nworld\n\"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(literal_values(&parts), vec!["hello\nworld"]);
}

#[test]
fn test_multiline_escapes_in_parser() {
    let src = "x = \"\"\"\n  hello\\tworld\n  \"\"\"\n";
    let parts = parse_string_parts(src);
    assert_eq!(literal_values(&parts), vec!["hello\tworld"]);
}
