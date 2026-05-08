//! Shared test scaffolding for the alpha-typecheck integration test
//! suite. Each `tests/*.rs` file is a separate Cargo test binary, so
//! anything pulled in here lives behind a `mod common;` in the test
//! file. The directory form (`tests/common/mod.rs` rather than
//! `tests/common.rs`) keeps Cargo from picking this up as a test
//! target itself.
//!
//! Today's surface is intentionally narrow: every test wants to drive
//! `parse_program → check_program` against a single in-memory source
//! file, so we expose:
//!
//! - [`PACKAGE`] — the package name every test source registers under.
//! - [`typecheck_file`] / [`typecheck_file_fail`] — happy and failure
//!   shorthands for `ParseMode::File` (the most common shape — a
//!   source containing `fn main` and friends).
//! - [`typecheck_script`] / [`typecheck_script_fail`] — same shape
//!   for `ParseMode::Script` (top-level statements).
//! - [`typecheck`] / [`typecheck_fail`] / [`parse_and_check`] — the
//!   raw `(source, mode)` versions that the shorthands route through;
//!   exposed so tests covering both modes don't need a per-mode shim.
//! - [`diagnostic_messages`] — flatten a [`CheckFailure`] to its raw
//!   message strings for the `.contains(...)` assertions every
//!   negative test ends in.
//! - [`warning_messages`] — flatten a [`CheckedProgram`]'s success-
//!   path diagnostics to message strings so tests asserting on
//!   warning-severity output (Phase 5 reachability) can compare
//!   against the raw text.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program};
use expo_ast::ast::Severity;
use expo_parser::{ParseMode, SourceFile, parse_program};

pub const PACKAGE: &str = "TestApp";

pub fn typecheck_file(source: &str) -> CheckedProgram {
    typecheck(source, ParseMode::File)
}

pub fn typecheck_file_fail(source: &str) -> CheckFailure {
    typecheck_fail(source, ParseMode::File)
}

pub fn typecheck_script(source: &str) -> CheckedProgram {
    typecheck(source, ParseMode::Script)
}

pub fn typecheck_script_fail(source: &str) -> CheckFailure {
    typecheck_fail(source, ParseMode::Script)
}

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    parse_and_check(source, mode).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed on `{source}`: {} diagnostic(s):\n{failure}",
            failure.diagnostics.len()
        )
    })
}

pub fn typecheck_fail(source: &str, mode: ParseMode) -> CheckFailure {
    parse_and_check(source, mode).expect_err(
        "expected alpha typecheck to fail; it succeeded (test source must produce a diagnostic)",
    )
}

pub fn parse_and_check(source: &str, mode: ParseMode) -> Result<CheckedProgram, CheckFailure> {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("test.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed)
}

pub fn diagnostic_messages(failure: &CheckFailure) -> Vec<String> {
    failure
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

pub fn warning_messages(checked: &CheckedProgram) -> Vec<String> {
    checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message.clone())
        .collect()
}
