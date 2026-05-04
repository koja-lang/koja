//! Lowering coverage for the boolean and comparison operators:
//! `and`, `or`, `not`, `== != < > <= >=`, and unary `-`.
//!
//! Mirrors the `two_plus_two.rs` pattern: parse + check + lower a
//! small `fn main`, then assert the produced instruction sequence
//! matches the eager lowering contract (both operands first, then a
//! single `BinaryOp` or `UnaryOp`).

use std::path::PathBuf;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRTerminator,
    IRUnaryOp, ValueId, lower_program,
};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("boolean_ops.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn entry_block(program: &IRProgram) -> &IRBasicBlock {
    let main: &IRFunction = program.entry_function();
    assert_eq!(main.blocks.len(), 1, "POC fns lower to one basic block");
    &main.blocks[0]
}

#[test]
fn and_lowers_to_two_consts_and_a_binary_op() {
    let program = lower("fn main\n  true and false\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Bool(true),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Bool(false),
            },
            IRInstruction::BinaryOp {
                dest: ValueId(2),
                lhs: ValueId(0),
                op: IRBinOp::And,
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
fn or_lowers_to_ir_bin_op_or() {
    let program = lower("fn main\n  true or false\nend\n");
    let block = entry_block(&program);
    assert!(matches!(
        block.instructions.last(),
        Some(IRInstruction::BinaryOp {
            op: IRBinOp::Or,
            ..
        }),
    ));
}

#[test]
fn not_lowers_to_unary_op_not() {
    let program = lower("fn main\n  not true\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Bool(true),
            },
            IRInstruction::UnaryOp {
                dest: ValueId(1),
                op: IRUnaryOp::Not,
                operand: ValueId(0),
            },
        ],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(1)),
        },
    );
}

#[test]
fn neg_lowers_to_unary_op_neg() {
    let program = lower("fn main\n  -7\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Int(7),
            },
            IRInstruction::UnaryOp {
                dest: ValueId(1),
                op: IRUnaryOp::Neg,
                operand: ValueId(0),
            },
        ],
    );
}

#[test]
fn comparisons_lower_to_matching_ir_bin_ops() {
    for (source, expected) in [
        ("fn main\n  1 == 2\nend\n", IRBinOp::Eq),
        ("fn main\n  1 != 2\nend\n", IRBinOp::NotEq),
        ("fn main\n  1 < 2\nend\n", IRBinOp::Lt),
        ("fn main\n  1 > 2\nend\n", IRBinOp::Gt),
        ("fn main\n  1 <= 2\nend\n", IRBinOp::LtEq),
        ("fn main\n  1 >= 2\nend\n", IRBinOp::GtEq),
    ] {
        let program = lower(source);
        let block = entry_block(&program);
        let Some(IRInstruction::BinaryOp { op, .. }) = block.instructions.last() else {
            panic!("expected trailing BinaryOp for source {source:?}");
        };
        assert_eq!(*op, expected, "source = {source:?}");
    }
}
