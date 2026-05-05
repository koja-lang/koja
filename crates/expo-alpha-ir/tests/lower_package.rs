//! Coverage for the package- and function-shaped lowering entry
//! points (`src/lower/package.rs`):
//!
//! - happy-path lowering of a `fn main` (project mode) and a bare
//!   trailing-expression script body, asserting the produced
//!   instruction sequence on the resulting [`IRProgram`] /
//!   [`IRScript`];
//! - cross-package call wiring through script mode's
//!   [`IRScript::function`] index;
//! - the [`LowerError::EntryPointNotFound`] failure mode for a
//!   missing `fn main`;
//! - the per-function fail-fast contract: extern-fn-without-body
//!   surfaces a feature-gap diagnostic, and a single bad function
//!   in a multi-fn package produces exactly one diagnostic for the
//!   failing function while the rest are still walked.

use std::path::PathBuf;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRScript,
    IRTerminator, IRType, LowerError, ValueId, lower_program, lower_script,
};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_package.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

fn lower(source: &str) -> IRProgram {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn lower_as_script(source: &str) -> IRScript {
    let checked = typecheck(source, ParseMode::Script);
    lower_script(&checked).expect("script lowering should succeed")
}

fn lower_err(source: &str, entry: &str) -> LowerError {
    let checked = typecheck(source, ParseMode::File);
    let entry_id = Identifier::new(PACKAGE, vec![entry.to_string()]);
    lower_program(&checked, entry_id).expect_err("lowering should surface diagnostics")
}

fn expect_diagnostics(err: LowerError) -> Vec<String> {
    match err {
        LowerError::Diagnostics(d) => d.into_iter().map(|diag| diag.message).collect(),
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}

#[test]
fn fn_main_two_plus_two_lowers_to_const_const_add_return() {
    let source = "
        fn main
          2 + 2
        end
        ";

    let program = lower(&dedent(source));

    assert_eq!(program.entry_point.mangled(), format!("{PACKAGE}.main"));
    assert_eq!(program.packages.len(), 1);
    let pkg = &program.packages[0];
    assert_eq!(pkg.package, PACKAGE);
    assert_eq!(pkg.functions.len(), 1);

    let main: &IRFunction = program.entry_function();
    assert_eq!(main.symbol, program.entry_point);
    assert_eq!(main.blocks.len(), 1, "fns lower to one basic block");

    let block: &IRBasicBlock = &main.blocks[0];
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Int64(2),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Int64(2),
            },
            IRInstruction::BinaryOp {
                dest: ValueId(2),
                lhs: ValueId(0),
                op: IRBinOp::Add,
                rhs: ValueId(1),
            },
        ],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(2)),
        },
    );
}

#[test]
fn bare_two_plus_two_lowers_to_script_with_const_const_add_return() {
    let script = lower_as_script("2 + 2\n");

    assert_eq!(
        script.return_type,
        IRType::Int64,
        "trailing `2 + 2` should stamp Int64 on IRScript.return_type",
    );
    assert!(
        script.packages.iter().all(|pkg| pkg.functions.is_empty()),
        "no helper fns expected in this fixture; got {:?}",
        script.packages,
    );
    assert_eq!(
        script.blocks.len(),
        1,
        "script bodies lower to a single basic block",
    );

    let block = &script.blocks[0];
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Int64(2),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Int64(2),
            },
            IRInstruction::BinaryOp {
                dest: ValueId(2),
                lhs: ValueId(0),
                op: IRBinOp::Add,
                rhs: ValueId(1),
            },
        ],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(2)),
        },
    );
}

#[test]
fn script_with_helper_fn_lowers_call_through_packages() {
    let script = lower_as_script("fn helper -> Int\n  1\nend\n\nhelper() + 1\n");

    let helper_mangled = format!("{PACKAGE}.helper");
    let helper = script
        .function(&helper_mangled)
        .expect("helper fn should be lowered into IRScript.packages");
    assert_eq!(helper.symbol.mangled(), helper_mangled);
    assert_eq!(helper.return_type, IRType::Int64);

    assert_eq!(script.return_type, IRType::Int64);
    let block = &script.blocks[0];
    let trailing = block
        .instructions
        .last()
        .expect("script body should produce at least one instruction");
    assert!(
        matches!(
            trailing,
            IRInstruction::BinaryOp {
                op: IRBinOp::Add,
                ..
            },
        ),
        "expected trailing BinaryOp(Add), got {trailing:?}",
    );
    let call = block
        .instructions
        .iter()
        .find(|inst| matches!(inst, IRInstruction::Call { .. }))
        .expect("expected a Call instruction in the script body");
    let IRInstruction::Call { callee, .. } = call else {
        unreachable!();
    };
    assert_eq!(callee.mangled(), helper_mangled);
}

#[test]
fn lower_program_reports_missing_entry_point() {
    let source = "
        fn other
          1
        end
        ";

    let program = dedent(source);
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("no_main.expo"),
            source: program,
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).expect("typecheck should succeed");
    let missing = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let err = lower_program(&checked, missing.clone())
        .expect_err("missing entry point should be reported");
    match err {
        LowerError::EntryPointNotFound { identifier } => assert_eq!(identifier, missing),
        other => panic!("expected EntryPointNotFound, got {other:?}"),
    }
}

#[test]
fn extern_fn_without_body_surfaces_feature_gap_diagnostic() {
    let source = "
        @extern \"C\"
        fn missing() -> Int
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "missing"));
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("extern fn `missing`"),
        "expected extern-fn diagnostic, got: {messages:?}",
    );
}

/// When one function fails to lower, other functions in the same
/// package still get walked — the failing one is simply omitted, and
/// the final [`LowerError::Diagnostics`] carries *only* the diagnostic
/// from the function that actually failed. Pins the per-function
/// fail-fast contract so a single bad function doesn't mask issues in
/// other ones and doesn't spew spurious errors either.
#[test]
fn partial_failure_reports_only_the_failing_function_diagnostic() {
    let source = "
        fn main
          1
        end

        fn broken
          x = 1
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(
        messages.len(),
        1,
        "expected a single diagnostic from the failing fn, got: {messages:?}",
    );
    assert!(
        messages[0].contains("assignment statements"),
        "expected assignment-statement diagnostic from `broken`, got: {messages:?}",
    );
}
