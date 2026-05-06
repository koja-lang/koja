//! Coverage for the operator + literal helpers in `src/lower/ops.rs`:
//! `lower_literal`, `lower_bin_op`, `lower_unary_op`, and the
//! `*_result_type` helpers (observed indirectly through the
//! produced `IRInstruction` shape).
//!
//! The eager-lowering contract: both operands first, then a single
//! `BinaryOp` / `UnaryOp` for the result.

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRTerminator,
    IRUnaryOp, ValueId,
};

mod common;

use common::{lower_program_source as lower, lower_script_source as lower_as_script};

fn entry_block(program: &IRProgram) -> &IRBasicBlock {
    let main: &IRFunction = program.entry_function();
    assert_eq!(main.blocks.len(), 1, "fns lower to one basic block");
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
                value: ConstValue::Int64(7),
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
fn script_mode_and_lowers_to_two_consts_and_a_binary_op() {
    let script = lower_as_script("true and false\n");
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

#[test]
fn float_literal_lowers_to_const_float64() {
    let program = lower("fn main\n  1.5\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![IRInstruction::Const {
            dest: ValueId(0),
            value: ConstValue::Float64(1.5),
        }],
    );
    assert_eq!(
        block.terminator,
        IRTerminator::Return {
            value: Some(ValueId(0)),
        },
    );
}

#[test]
fn float_arithmetic_lowers_with_float64_operand_type() {
    let program = lower("fn main\n  2.0 + 2.0\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![
            IRInstruction::Const {
                dest: ValueId(0),
                value: ConstValue::Float64(2.0),
            },
            IRInstruction::Const {
                dest: ValueId(1),
                value: ConstValue::Float64(2.0),
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
fn float_comparison_lowers_with_bool_result() {
    let program = lower("fn main\n  1.0 < 2.0\nend\n");
    let block = entry_block(&program);
    let Some(IRInstruction::BinaryOp { op, lhs, rhs, .. }) = block.instructions.last() else {
        panic!(
            "expected trailing BinaryOp for `1.0 < 2.0`, got {:?}",
            block.instructions.last(),
        );
    };
    assert_eq!(*op, IRBinOp::Lt);
    // Operands trace back to the two preceding Float64 consts.
    assert_eq!(
        block.instructions[..2],
        [
            IRInstruction::Const {
                dest: *lhs,
                value: ConstValue::Float64(1.0),
            },
            IRInstruction::Const {
                dest: *rhs,
                value: ConstValue::Float64(2.0),
            },
        ],
    );
}
