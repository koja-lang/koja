//! Shared test scaffolding for the parser integration suite. Each
//! `tests/*.rs` file is a separate Cargo test binary, so anything
//! pulled in here lives behind a `mod common;` in the test file.
//! The directory form (`tests/common/mod.rs` rather than
//! `tests/common.rs`) keeps Cargo from picking this up as a test
//! target itself.
//!
//! Conventions:
//! - Every driver and extractor dedents its source internally, so
//!   call sites pass the raw indented literal with no
//!   `dedent(...)` wrapper.
//! - Happy-path tests call [`parse_clean`] / [`parse_clean_script`]
//!   or a `first_*` extractor, all of which panic on any diagnostic.
//! - Negative tests use [`parse_failing_with`], which asserts each
//!   needle appears in some diagnostic message (substring-based so
//!   wording can be tightened without invalidating dozens of tests)
//!   and returns the result for further hint / recovery assertions.

#![allow(dead_code)]

use koja_ast::ast::{
    AliasDecl, Constant, EnumDecl, Expr, ExprKind, ExtendBlock, File, Function, ImplBlock, Item,
    MatchArm, ProtocolDecl, Statement, StructDecl, TypeExpr,
};
use koja_ast::util::dedent;
use koja_parser::{ParseMode, ParseResult, parse};

/// Parse dedented `source` in `ParseMode::File` and assert no
/// diagnostics.
pub fn parse_clean(source: &str) -> File {
    let result = parse(&dedent(source), ParseMode::File);
    assert!(
        result.errors.is_empty(),
        "expected clean parse, got errors:\n{}",
        format_errors(&result),
    );
    result.ast
}

/// Parse dedented `source` in `ParseMode::Script` and assert no
/// diagnostics.
pub fn parse_clean_script(source: &str) -> File {
    let result = parse(&dedent(source), ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "expected clean parse, got errors:\n{}",
        format_errors(&result),
    );
    result.ast
}

/// Parse dedented `source` in `ParseMode::File`, expecting at least
/// one diagnostic. Returns the whole result for substring assertions.
pub fn parse_failing(source: &str) -> ParseResult {
    let result = parse(&dedent(source), ParseMode::File);
    assert!(
        !result.errors.is_empty(),
        "expected parse errors, got a clean parse",
    );
    result
}

/// [`parse_failing`] plus a message assertion per needle. Returns the
/// result so hint / recovery assertions can follow.
pub fn parse_failing_with(source: &str, needles: &[&str]) -> ParseResult {
    let result = parse_failing(source);
    for needle in needles {
        assert_message_contains(&result, needle);
    }
    result
}

pub fn error_messages(result: &ParseResult) -> Vec<String> {
    result.errors.iter().map(|d| d.message.clone()).collect()
}

pub fn error_hints(result: &ParseResult) -> Vec<String> {
    result
        .errors
        .iter()
        .filter_map(|d| d.hint.clone())
        .collect()
}

/// Assert at least one diagnostic message contains `needle`.
pub fn assert_message_contains(result: &ParseResult, needle: &str) {
    let messages = error_messages(result);
    assert!(
        messages.iter().any(|m| m.contains(needle)),
        "expected an error message containing `{needle}`, got:\n{}",
        format_errors(result),
    );
}

/// Assert at least one diagnostic hint contains `needle`.
pub fn assert_hint_contains(result: &ParseResult, needle: &str) {
    let hints = error_hints(result);
    assert!(
        hints.iter().any(|h| h.contains(needle)),
        "expected a hint containing `{needle}`, got:\n{}",
        format_errors(result),
    );
}

fn format_errors(result: &ParseResult) -> String {
    result
        .errors
        .iter()
        .map(|d| match &d.hint {
            Some(hint) => format!("  - {} (hint: {hint})", d.message),
            None => format!("  - {}", d.message),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_item<T>(source: &str, what: &str, extract: impl FnMut(Item) -> Option<T>) -> T {
    parse_clean(source)
        .items
        .into_iter()
        .find_map(extract)
        .unwrap_or_else(|| panic!("no {what} in parsed output"))
}

macro_rules! first_item_fn {
    ($name:ident, $variant:ident, $ty:ty, $what:literal) => {
        pub fn $name(source: &str) -> $ty {
            first_item(source, $what, |item| match item {
                Item::$variant(inner) => Some(inner),
                _ => None,
            })
        }
    };
}

first_item_fn!(first_alias, Alias, AliasDecl, "alias");
first_item_fn!(first_constant, Constant, Constant, "constant");
first_item_fn!(first_enum, Enum, EnumDecl, "enum");
first_item_fn!(first_extend, Extend, ExtendBlock, "extend block");
first_item_fn!(first_function, Function, Function, "function");
first_item_fn!(first_impl, Impl, ImplBlock, "impl block");
first_item_fn!(first_protocol, Protocol, ProtocolDecl, "protocol");
first_item_fn!(first_struct, Struct, StructDecl, "struct");

/// First expression in `body`, looking through assignment values.
pub fn body_expr(body: Vec<Statement>) -> Option<Expr> {
    body.into_iter().find_map(|stmt| match stmt {
        Statement::Assignment { value, .. } | Statement::Expr(value) => Some(value),
        _ => None,
    })
}

/// First expression (or assignment value) in the first function's
/// body.
pub fn first_function_expr(source: &str) -> Expr {
    let body = first_function(source).body.unwrap_or_default();
    body_expr(body).expect("no expression in parsed output")
}

/// First expression (or assignment value) at the top level of a
/// script-mode source. Tolerates one-liner sources with no trailing
/// newline.
pub fn first_script_expr(source: &str) -> Expr {
    let body = parse_clean_script(&format!("{source}\n"))
        .body
        .unwrap_or_default();
    body_expr(body).expect("no expression in parsed output")
}

/// Arms of the first `match` expression in the first function's body.
pub fn first_match_arms(source: &str) -> Vec<MatchArm> {
    let expr = first_function_expr(source);
    match expr.kind {
        ExprKind::Match { arms, .. } => arms,
        other => panic!("expected a match expression, got {other:?}"),
    }
}

/// Declared type of the first field of the first struct.
pub fn first_field_type(source: &str) -> TypeExpr {
    first_struct(source)
        .fields
        .into_iter()
        .next()
        .expect("struct has no fields")
        .type_expr
}
