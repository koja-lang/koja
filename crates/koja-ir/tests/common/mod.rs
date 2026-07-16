//! Shared test scaffolding for the ir integration test suite.
//! Each `tests/*.rs` file is its own Cargo test binary, so anything
//! pulled in here lives behind a `mod common;` in the test file. The
//! directory form (`tests/common/mod.rs`) keeps Cargo from picking
//! this up as a test target itself.
//!
//! Every ir test shape drives `parse -> check -> lower` against a
//! single in-memory source file. All drivers dedent the source
//! internally, so call sites pass raw indented literals.
//!
//! - [`PACKAGE`] is the default package every test source registers
//!   under (`"TestApp"`).
//! - [`typecheck`] / [`typecheck_in`] are `parse_program -> check_program`
//!   shorthands, parameterized by `ParseMode` (and optionally package
//!   name for tests that target `Global` directly).
//! - [`lower_program_source`] / [`lower_script_source`] /
//!   [`lower_script_source_in`] are happy-path lowering shorthands.
//!   Program-shaped fixtures get the synthetic [`TEST_ENTRY_NAME`]
//!   Process state appended so `lower_program` always has a valid
//!   Process entry. Fixture functions (`fn main` and friends) lower
//!   as plain package helpers alongside it.
//! - [`lower_program_err`] drives `lower_program` to its error arm
//!   and unwraps the [`LowerError`].
//! - [`expect_diagnostics`] flattens a [`LowerError::Diagnostics`]
//!   to its raw message strings for `.contains(...)` assertions.
//! - [`function`] / [`script_function`] / [`mangled_function`] fetch
//!   an [`IRFunction`] by name, panicking if missing.
//! - Block scanners ([`entry_block`], [`block_labeled`],
//!   [`all_instructions`], [`count_blocks_with_prefix`],
//!   [`branch_targets_into`], [`local_decls`]) cover the common CFG
//!   assertions over a `&[IRBasicBlock]` slice, which works for both
//!   function bodies and script top-level blocks.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::{
    BranchTarget, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRProgram, IRScript,
    IRTerminator, LowerError, lower_program, lower_script,
};
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};

pub const PACKAGE: &str = "TestApp";

/// Name of the synthetic Process state appended to program fixtures.
pub const TEST_ENTRY_NAME: &str = "TestEntry";

/// Minimal `Process` impl appended to every program-shaped fixture
/// so `lower_program` has a valid entry. The state is never spawned
/// or executed by these tests, it only satisfies the entry staging.
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

/// Append the synthetic entry snippet to a program fixture. Dedents
/// the fixture first so the combined source is uniformly flush-left
/// (a later whole-string dedent would see the snippet's column-0
/// lines and leave the fixture indented).
pub fn with_test_entry(source: &str) -> String {
    format!("{}\n{TEST_ENTRY_SNIPPET}", dedent(source))
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
        source: dedent(source),
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
    mangled_function(script, &format!("{PACKAGE}.{name}"))
}

/// Fetch a function from an [`IRScript`] by its full mangled name,
/// for monomorphization tests that assert on mangled symbols
/// (`TestApp.id_$Int64$`).
pub fn mangled_function<'a>(script: &'a IRScript, mangled: &str) -> &'a IRFunction {
    script
        .function(mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRScript"))
}

/// All mangled function names in the script's packages, sorted.
pub fn script_function_names(script: &IRScript) -> Vec<String> {
    let mut names: Vec<String> = script
        .packages
        .iter()
        .flat_map(|package| package.functions.keys())
        .map(|symbol| symbol.mangled().to_string())
        .collect();
    names.sort();
    names
}

pub fn entry_block(blocks: &[IRBasicBlock]) -> &IRBasicBlock {
    blocks.first().expect("body should have at least one block")
}

/// Find the unique block with an exact `label`, panicking if absent.
pub fn block_labeled<'a>(blocks: &'a [IRBasicBlock], label: &str) -> &'a IRBasicBlock {
    blocks
        .iter()
        .find(|block| block.label == label)
        .unwrap_or_else(|| panic!("missing block labeled `{label}`"))
}

pub fn count_blocks_with_prefix(blocks: &[IRBasicBlock], prefix: &str) -> usize {
    blocks
        .iter()
        .filter(|block| block.label.starts_with(prefix))
        .count()
}

/// Every instruction across all blocks, in block order.
pub fn all_instructions(blocks: &[IRBasicBlock]) -> impl Iterator<Item = &IRInstruction> {
    blocks.iter().flat_map(|block| block.instructions.iter())
}

/// All `LocalDecl` instructions across the body, in block order.
pub fn local_decls(blocks: &[IRBasicBlock]) -> Vec<&IRInstruction> {
    all_instructions(blocks)
        .filter(|instruction| matches!(instruction, IRInstruction::LocalDecl { .. }))
        .collect()
}

/// Unconditional-`Branch` edges into `target` (the merge-block
/// convergence assertion most CFG tests end in).
pub fn branch_targets_into(blocks: &[IRBasicBlock], target: IRBlockId) -> Vec<&BranchTarget> {
    blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            IRTerminator::Branch(edge) if edge.block == target => Some(edge),
            _ => None,
        })
        .collect()
}
