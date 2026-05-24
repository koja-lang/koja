//! Shared test scaffolding for the parser integration suite. Each
//! `tests/*.rs` file is a separate Cargo test binary, so anything
//! pulled in here lives behind a `mod common;` in the test file.
//! The directory form (`tests/common/mod.rs` rather than
//! `tests/common.rs`) keeps Cargo from picking this up as a test
//! target itself.
//!
//! Conventions:
//! - Source fixtures should be spelled with `koja_ast::util::dedent`
//!   so they line up with surrounding test code.
//! - Happy-path tests call [`parse_clean`] / [`parse_clean_script`]
//!   which panic on any diagnostic — the assertion stays at the
//!   call site and the AST returns ready to inspect.
//! - Negative tests collect [`error_messages`] and use
//!   `.iter().any(|m| m.contains(...))` rather than exact equality
//!   so we can tweak wording without invalidating dozens of tests.

#![allow(dead_code)]

use koja_ast::ast::File;
use koja_parser::{ParseMode, ParseResult, parse};

/// Parse `source` in `ParseMode::File` and assert no diagnostics.
pub fn parse_clean(source: &str) -> File {
    let result = parse(source, ParseMode::File);
    assert!(
        result.errors.is_empty(),
        "expected clean parse, got errors:\n{}",
        format_errors(&result),
    );
    result.ast
}

/// Parse `source` in `ParseMode::Script` and assert no diagnostics.
pub fn parse_clean_script(source: &str) -> File {
    let result = parse(source, ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "expected clean parse, got errors:\n{}",
        format_errors(&result),
    );
    result.ast
}

/// Parse `source` in `ParseMode::File`, expecting at least one
/// diagnostic. Returns the whole result for substring assertions.
pub fn parse_failing(source: &str) -> ParseResult {
    let result = parse(source, ParseMode::File);
    assert!(
        !result.errors.is_empty(),
        "expected parse errors, got a clean parse",
    );
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
