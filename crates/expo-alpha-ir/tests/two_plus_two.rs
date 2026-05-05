//! End-to-end smoke tests for the alpha IR lowering pipeline at its
//! POC scope.
//!
//! Drives `parse_program → check_program → lower_{program,script}` on
//! `2 + 2` in both shapes:
//!
//! - **Project mode**: source is `fn main; 2 + 2; end`, lowered via
//!   [`lower_program`]. Asserts the resulting [`IRProgram`] has the
//!   exact instruction sequence an interpreter can execute to produce
//!   `Int(4)`.
//! - **Script mode**: source is bare `2 + 2\n`, lowered via
//!   [`lower_script`]. Asserts the resulting [`IRScript`] has the
//!   same `2 + 2 → Return` shape, but with no entry-point identifier
//!   and the body sitting directly on `IRScript.blocks` instead of
//!   inside an [`IRFunction`].
//!
//! Together they cover both paths from `expo-alpha-ir`'s public
//! surface; downstream backends pick the matching `run_*` /
//! `compile_*` entry point.

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
            path: PathBuf::from("two_plus_two.expo"),
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

#[test]
fn fn_main_two_plus_two_lowers_to_const_const_add_return() {
    let source = "
        fn main
          2 + 2
        end
        ";

    let program = lower(&dedent(source));

    assert_eq!(program.entry_point.last(), "main");
    assert_eq!(program.packages.len(), 1);
    let pkg = &program.packages[0];
    assert_eq!(pkg.package, PACKAGE);
    assert_eq!(pkg.functions.len(), 1);

    let main: &IRFunction = program.entry_function();
    assert_eq!(main.identifier, program.entry_point);
    assert_eq!(main.blocks.len(), 1, "POC fns lower to one basic block");

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
        "POC script bodies lower to a single basic block",
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

    let helper_id = Identifier::new(PACKAGE, vec!["helper".to_string()]);
    let helper = script
        .function(&helper_id)
        .expect("helper fn should be lowered into IRScript.packages");
    assert_eq!(helper.identifier, helper_id);
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
    assert_eq!(*callee, helper_id);
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
