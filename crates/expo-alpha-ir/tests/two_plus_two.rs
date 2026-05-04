//! End-to-end smoke test for the alpha IR lowering pipeline at its POC scope.
//!
//! Drives `parse_program → check_program → lower_program` on
//! `fn main; 2 + 2; end` and asserts the resulting `IRProgram` has the
//! exact instruction sequence that an interpreter can execute to
//! produce `Int(4)`.
//!
//! When this test passes, the alpha pipeline (typecheck + IR) is
//! end-to-end ready for `expo-alpha-ir-eval` consumption.

use std::path::PathBuf;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRTerminator,
    LowerError, ValueId, lower_program,
};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("two_plus_two.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

#[test]
fn fn_main_two_plus_two_lowers_to_const_const_add_return() {
    let program = lower("fn main\n  2 + 2\nend\n");

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
                value: ConstValue::Int(2),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Int(2),
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
fn lower_program_reports_missing_entry_point() {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("no_main.expo"),
            source: "fn other\n  1\nend\n".to_string(),
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
