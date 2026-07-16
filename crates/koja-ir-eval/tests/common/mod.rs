//! Shared test scaffolding for the ir-eval integration test
//! suite. Each `tests/*.rs` file is its own Cargo test binary, so
//! anything pulled in here lives behind a `mod common;` in the test
//! file. The directory form (`tests/common/mod.rs`) keeps Cargo from
//! picking this up as a test target itself.
//!
//! Every eval test shape is a `parse -> check -> lower -> run` chain
//! against a single in-memory source file, so we expose:
//!
//! - [`PACKAGE`]: the default package every test source registers
//!   under (`"TestApp"`).
//! - [`typecheck`] / [`typecheck_in`]: `parse_program -> check_program`
//!   shorthands, parameterized by `ParseMode` (and optionally
//!   package name for tests that want to target `Global` directly).
//! - [`evaluate_program`]: `ParseMode::File` + `lower_program`
//!   (with a synthetic Process entry appended so the entry staging
//!   succeeds) + `Interpreter::run_function` against the fixture's
//!   `fn main`, returning its runtime [`Value`].
//! - [`evaluate_script`] / [`evaluate_script_in`]: `ParseMode::Script`
//!   + `lower_script` + `Interpreter::run_script`. The trailing
//!     expression's runtime [`Value`] becomes the script's return,
//!     which is what every script-shaped assertion inspects.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU16, Ordering};

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::{lower_program, lower_script};
use koja_ir_eval::{Interpreter, RuntimeError, Value};
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckedProgram, check_program};

pub const PACKAGE: &str = "TestApp";

/// Name of the synthetic Process state appended to program fixtures.
pub const TEST_ENTRY_NAME: &str = "TestEntry";

/// Minimal `Process` impl appended to every program-shaped fixture
/// so `lower_program` has a valid entry. The state is never spawned
/// or executed by these tests. It only satisfies the entry staging.
/// [`evaluate_program`] runs the fixture's `fn main` directly.
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

/// Sequential per-test port offset on top of a pid-derived base, so
/// parallel test threads (and concurrent test processes) bind
/// distinct loopback ports.
static PORT_OFFSET: AtomicU16 = AtomicU16::new(0);

pub fn fresh_port() -> u16 {
    let base = 20000 + (process::id() % 20000) as u16;
    base + PORT_OFFSET.fetch_add(1, Ordering::Relaxed)
}

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    typecheck_in(PACKAGE, source, mode)
}

pub fn typecheck_in(package: &str, source: &str, mode: ParseMode) -> CheckedProgram {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.push(test_source(package, source));
    let parsed = parse_program(sources, mode);
    check_program(parsed).unwrap_or_else(|failure| panic!("typecheck failed:\n{failure}"))
}

pub fn evaluate_program(source: &str) -> Result<Value, RuntimeError> {
    run_main(typecheck(&source_with_entry(source), ParseMode::File))
}

/// [`evaluate_program`] variant that additionally bundles the
/// qualified stdlib packages (`Net`, …) into the compilation unit,
/// for fixtures that exercise `alias Net.*` surfaces.
pub fn evaluate_qualified_program(source: &str) -> Result<Value, RuntimeError> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    sources.push(test_source(PACKAGE, &source_with_entry(source)));
    let parsed = parse_program(sources, ParseMode::File);
    let checked =
        check_program(parsed).unwrap_or_else(|failure| panic!("typecheck failed:\n{failure}"));
    run_main(checked)
}

fn source_with_entry(source: &str) -> String {
    format!("{source}\n{}", dedent(TEST_ENTRY_SNIPPET))
}

fn test_source(package: &str, source: &str) -> SourceFile {
    SourceFile {
        package: package.to_string(),
        path: PathBuf::from("test.koja"),
        source: source.to_string(),
    }
}

fn run_main(checked: CheckedProgram) -> Result<Value, RuntimeError> {
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
