//! Shared test scaffolding for the ir integration test suite.
//! Each `tests/*.rs` file is its own Cargo test binary, so anything
//! pulled in here lives behind a `mod common;` in the test file. The
//! directory form (`tests/common/mod.rs`) keeps Cargo from picking
//! this up as a test target itself.
//!
//! Every ir test shape drives `parse -> check -> lower` against a
//! single in-memory source file, so we expose:
//!
//! - [`PACKAGE`] — the default package every test source registers
//!   under (`"TestApp"`).
//! - [`typecheck`] / [`typecheck_in`] — `parse_program -> check_program`
//!   shorthands, parameterized by `ParseMode` (and optionally package
//!   name for tests that target `Global` directly, e.g.
//!   `lower_intrinsics.rs`).
//! - [`lower_program_source`] / [`lower_script_source`] /
//!   [`lower_script_source_in`] — happy-path lowering shorthands.
//!   Program-shaped fixtures get the synthetic [`TEST_ENTRY_NAME`]
//!   Process state appended so `lower_program` always has a valid
//!   Process entry; fixture functions (`fn main` and friends) lower
//!   as plain package helpers alongside it.
//! - [`lower_program_err`] — drive `lower_program` to its error arm
//!   and unwrap the [`LowerError`].
//! - [`expect_diagnostics`] — flatten a [`LowerError::Diagnostics`]
//!   to its raw message strings for the `.contains(...)` assertions
//!   negative tests end in.
//! - [`function`] — fetch an [`IRFunction`] from an [`IRProgram`] by
//!   its short (unmangled) name, panicking if missing.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;

use koja_ast::identifier::Identifier;
use koja_ir::{IRFunction, IRProgram, IRScript, LowerError, lower_program, lower_script};
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};

pub const PACKAGE: &str = "TestApp";

/// Name of the synthetic Process state appended to program fixtures.
pub const TEST_ENTRY_NAME: &str = "TestEntry";

/// Minimal `Process` impl appended to every program-shaped fixture
/// so `lower_program` has a valid entry. The state is never spawned
/// or executed by these tests — it only satisfies the entry staging.
const TEST_ENTRY_SNIPPET: &str = "
struct TestEntry
end

impl Process<(), (), ()> for TestEntry
  fn start(config: ()) -> Result<Self, Process.StopReason>
    Result.Ok(TestEntry{})
  end

  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<Self>
    Process.Step.Continue(self)
  end
end
";

/// Append the synthetic entry snippet to a program fixture.
pub fn with_test_entry(source: &str) -> String {
    format!("{source}\n{TEST_ENTRY_SNIPPET}")
}

/// The synthetic entry's identifier (`TestApp.TestEntry`).
pub fn test_entry_identifier() -> Identifier {
    Identifier::new(PACKAGE, vec![TEST_ENTRY_NAME.to_string()])
}

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    typecheck_in(PACKAGE, source, mode)
}

pub fn typecheck_in(package: &str, source: &str, mode: ParseMode) -> CheckedProgram {
    parse_and_check(package, source, mode).unwrap_or_else(|f| panic!("typecheck failed:\n{f}"))
}

pub fn typecheck_fail(source: &str, mode: ParseMode) -> CheckFailure {
    typecheck_fail_in(PACKAGE, source, mode)
}

pub fn typecheck_fail_in(package: &str, source: &str, mode: ParseMode) -> CheckFailure {
    parse_and_check(package, source, mode).expect_err(
        "expected typecheck to fail; it succeeded (test source must produce a diagnostic)",
    )
}

fn parse_and_check(
    package: &str,
    source: &str,
    mode: ParseMode,
) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: package.to_string(),
        path: PathBuf::from("test.koja"),
        source: source.to_string(),
    });
    let parsed = parse_program(sources, mode);
    check_program(parsed)
}

pub fn lower_program_source(source: &str) -> IRProgram {
    let checked = typecheck(&with_test_entry(source), ParseMode::File);
    lower_program(&checked, &test_entry_identifier()).expect("lowering should succeed")
}

pub fn lower_script_source(source: &str) -> IRScript {
    lower_script_source_in(PACKAGE, source)
}

pub fn lower_script_source_in(package: &str, source: &str) -> IRScript {
    let checked = typecheck_in(package, source, ParseMode::Script);
    lower_script(&checked).expect("script lowering should succeed")
}

pub fn lower_program_err(source: &str) -> LowerError {
    let checked = typecheck(&with_test_entry(source), ParseMode::File);
    lower_program(&checked, &test_entry_identifier())
        .expect_err("lowering should surface diagnostics")
}

pub fn expect_diagnostics(err: LowerError) -> Vec<String> {
    match err {
        LowerError::Diagnostics(d) => d.into_iter().map(|diag| diag.message).collect(),
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}

pub fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRProgram"))
}

pub fn script_function<'a>(script: &'a IRScript, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    script
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRScript"))
}
