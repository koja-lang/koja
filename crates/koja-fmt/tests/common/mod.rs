//! Shared scaffolding for the formatter integration suite. Each
//! `tests/*.rs` file is a separate Cargo test binary, so anything pulled
//! in here lives behind a `mod common;` in the test file. The directory
//! form (`tests/common/mod.rs`) keeps Cargo from picking this up as a
//! test target itself.
//!
//! Every helper dedents its source, so call sites pass the raw indented
//! literal. `assert_fmt` checks input against expected output, and
//! `assert_unchanged` checks that a source is already canonical.

#![allow(dead_code)]

use koja_ast::util::dedent;
use koja_fmt::{FormatResult, format};
use koja_parser::ParseMode;

pub fn fmt(source: &str) -> String {
    fmt_mode(source, ParseMode::File)
}

pub fn fmt_script(source: &str) -> String {
    fmt_mode(source, ParseMode::Script)
}

fn fmt_mode(source: &str, mode: ParseMode) -> String {
    match format(&dedent(source), mode) {
        FormatResult::Ok(s) => s,
        FormatResult::ParseErrors(e) => panic!("parse error: {:?}", e),
    }
}

pub fn assert_fmt(input: &str, expected: &str) {
    assert_formatted(fmt(input), expected);
}

pub fn assert_fmt_script(input: &str, expected: &str) {
    assert_formatted(fmt_script(input), expected);
}

/// Assert `source` is already in canonical form (formatting it changes
/// nothing).
pub fn assert_unchanged(source: &str) {
    assert_fmt(source, source);
}

pub fn assert_unchanged_script(source: &str) {
    assert_fmt_script(source, source);
}

pub fn assert_formatted(actual: String, expected: &str) {
    let mut expected = dedent(expected);
    if !expected.ends_with('\n') {
        expected.push('\n');
    }
    assert_eq!(
        actual, expected,
        "\n--- actual ---\n{actual}--- expected ---\n{expected}"
    );
}
