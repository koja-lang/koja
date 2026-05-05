//! Coverage for the statement-list driver in `src/lower/body.rs`:
//!
//! - `Statement::Return` shape — both `return <expr>` and bare
//!   `return` (Unit) — pinning the terminator that gets stamped and
//!   the closed-flow contract;
//! - per-function fail-fast within the body itself: an unsupported
//!   `Statement::Assignment` halts the body walk after one
//!   diagnostic, and a body that mixes feature-gaps emits exactly
//!   one diagnostic for whichever gap trips first (no cascading).

use std::path::PathBuf;

use expo_alpha_ir::{
    IRFunction, IRInstruction, IRProgram, IRTerminator, IRType, LowerError, lower_program,
};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_body.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

fn lower(source: &str) -> IRProgram {
    let checked = typecheck(source);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn lower_err(source: &str, entry: &str) -> LowerError {
    let checked = typecheck(source);
    let entry_id = Identifier::new(PACKAGE, vec![entry.to_string()]);
    lower_program(&checked, entry_id).expect_err("lowering should surface diagnostics")
}

fn expect_diagnostics(err: LowerError) -> Vec<String> {
    match err {
        LowerError::Diagnostics(d) => d.into_iter().map(|diag| diag.message).collect(),
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}

fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRProgram"))
}

#[test]
fn explicit_return_with_value_terminates_block() {
    let source = "
        fn main
          return 7
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.blocks.len(),
        1,
        "a single explicit `return` produces one block",
    );

    let block = &main.blocks[0];
    let last = block.instructions.last().expect("expected a Const for `7`");
    let dest = last.dest();
    assert!(
        matches!(last, IRInstruction::Const { .. }),
        "trailing instruction should be Const(7); got {last:?}",
    );
    assert_eq!(block.terminator, IRTerminator::Return { value: Some(dest) });
}

#[test]
fn empty_main_body_returns_unit_with_no_value() {
    let source = "
        fn main
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(main.return_type, IRType::Unit);
    let block = main.blocks.first().expect("main has at least one block");
    assert!(
        block.instructions.is_empty(),
        "an empty body should not emit any instructions; got {:?}",
        block.instructions,
    );
    assert_eq!(block.terminator, IRTerminator::Return { value: None });
}

#[test]
fn assignment_statement_surfaces_feature_gap_diagnostic() {
    let source = "
        fn main
          x = 1
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("assignment statements"),
        "expected assignment diagnostic, got: {messages:?}",
    );
}

/// Multiple feature gaps inside a single function should emit *one*
/// diagnostic — the first one seen — and abort walking that function.
/// Pins the fail-fast-per-function contract explicitly: here the Float
/// literal trips first; if lowering kept walking it would also trip on
/// the assignment and produce two diagnostics instead of one.
#[test]
fn fail_fast_within_function_emits_single_diagnostic() {
    let source = "
        fn main
          1.5
          x = 2
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(
        messages.len(),
        1,
        "expected fail-fast within a function, got: {messages:?}",
    );
    assert!(
        messages[0].contains("Float literals"),
        "expected first diagnostic to be Float literal, got: {messages:?}",
    );
}
