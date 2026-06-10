//! Shared test scaffolding for the ir-eval integration test
//! suite. Each `tests/*.rs` file is its own Cargo test binary, so
//! anything pulled in here lives behind a `mod common;` in the test
//! file. The directory form (`tests/common/mod.rs`) keeps Cargo from
//! picking this up as a test target itself.
//!
//! Every eval test shape is a `parse → check → lower → run` chain
//! against a single in-memory source file, so we expose:
//!
//! - [`PACKAGE`] — the default package every test source registers
//!   under (`"TestApp"`).
//! - [`typecheck`] / [`typecheck_in`] — `parse_program → check_program`
//!   shorthands, parameterized by `ParseMode` (and optionally
//!   package name for tests that want to target `Global` directly).
//! - [`evaluate_program`] — `ParseMode::File` + `lower_program`
//!   (with a synthetic Process entry appended so the entry staging
//!   succeeds) + `Interpreter::run_function` against the fixture's
//!   `fn main`, returning its runtime [`Value`].
//! - [`evaluate_script`] / [`evaluate_script_in`] — `ParseMode::Script`
//!   + `lower_script` + `Interpreter::run_script`. The trailing
//!     expression's runtime [`Value`] becomes the script's return,
//!     which is what every script-shaped assertion inspects.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;

use koja_ast::identifier::Identifier;
use koja_ir::{lower_program, lower_script};
use koja_ir_eval::{Interpreter, RuntimeError, Value};
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckedProgram, check_program};

pub const PACKAGE: &str = "TestApp";

/// Name of the synthetic Process state appended to program fixtures.
pub const TEST_ENTRY_NAME: &str = "TestEntry";

/// Minimal `Process` impl appended to every program-shaped fixture
/// so `lower_program` has a valid entry. The state is never spawned
/// or executed by these tests — it only satisfies the entry staging;
/// [`evaluate_program`] runs the fixture's `fn main` directly.
const TEST_ENTRY_SNIPPET: &str = "
struct TestEntry
end

impl Process<(), (), ()> for TestEntry
  fn start(config: ()) -> Result<Self, StopReason>
    Result.Ok(TestEntry{})
  end

  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
    Step.Continue(self)
  end
end
";

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    typecheck_in(PACKAGE, source, mode)
}

pub fn typecheck_in(package: &str, source: &str, mode: ParseMode) -> CheckedProgram {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: package.to_string(),
        path: PathBuf::from("test.koja"),
        source: source.to_string(),
    });
    let parsed = parse_program(sources, mode);
    check_program(parsed).unwrap_or_else(|failure| panic!("typecheck failed:\n{failure}"))
}

pub fn evaluate_program(source: &str) -> Result<Value, RuntimeError> {
    let with_entry = format!("{source}\n{TEST_ENTRY_SNIPPET}");
    let checked = typecheck(&with_entry, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec![TEST_ENTRY_NAME.to_string()]);
    let program = lower_program(&checked, &entry).expect("lowering should succeed");
    Interpreter::run_function(&program, &format!("{PACKAGE}.main"))
}

pub fn evaluate_script(source: &str) -> Result<Value, RuntimeError> {
    evaluate_script_in(PACKAGE, source)
}

pub fn evaluate_script_in(package: &str, source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck_in(package, source, ParseMode::Script);
    let script = lower_script(&checked).expect("script lowering should succeed");
    Interpreter::run_script(&script)
}
