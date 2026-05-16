//! Coverage for the operator + literal helpers in `src/lower/ops.rs`:
//! `lower_literal`, `lower_bin_op`, `lower_unary_op`, and the
//! `*_result_type` helpers (observed indirectly through the
//! produced `IRInstruction` shape).
//!
//! The eager-lowering contract: both operands first, then a single
//! `BinaryOp` / `UnaryOp` for the result.

use expo_ir::{
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
fn hex_int_literal_lowers_with_correct_radix() {
    // The lexer hands lower the raw text `0xFF` (prefix preserved);
    // `lower/ops.rs::parse_int_literal` strips `0x` and dispatches
    // to `i64::from_str_radix(_, 16)`.
    let program = lower("fn main\n  0xFF\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![IRInstruction::Const {
            dest: ValueId(0),
            value: ConstValue::Int64(255),
        }],
    );
}

#[test]
fn binary_int_literal_lowers_with_correct_radix() {
    let program = lower("fn main\n  0b1010\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![IRInstruction::Const {
            dest: ValueId(0),
            value: ConstValue::Int64(0b1010),
        }],
    );
}

#[test]
fn underscore_separated_int_literal_strips_separators() {
    // `1_000_000` is decimal-with-underscores; the parser keeps the
    // underscores in the token text, so lower must strip them.
    let program = lower("fn main\n  1_000_000\nend\n");
    let block = entry_block(&program);
    assert_eq!(
        block.instructions,
        vec![IRInstruction::Const {
            dest: ValueId(0),
            value: ConstValue::Int64(1_000_000),
        }],
    );
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

// ---------------------------------------------------------------------------
// Narrow-int / narrow-float literal coercion: literal values flowing
// into a sized target slot mint `Const` instructions at the recorded
// width rather than the default `Int64` / `Float64` head. The tests
// span every literal-fit site (call arg, struct field, return) so a
// regression at any one site fails a dedicated case.
// ---------------------------------------------------------------------------

#[test]
fn call_arg_uint8_coerces_literal_to_const_uint8() {
    let program = lower("fn take(x: UInt8) -> Unit\n  ()\nend\n\nfn main\n  take(255)\nend\n");
    let block = entry_block(&program);
    assert!(
        block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::UInt8(255),
                ..
            }
        )),
        "expected `Const UInt8(255)` arg, got {:?}",
        block.instructions,
    );
}

#[test]
fn struct_field_int8_coerces_literal_to_const_int8() {
    let program =
        lower("struct Sample\n  amplitude: Int8\nend\n\nfn main\n  Sample{amplitude: -8}\nend\n");
    let block = entry_block(&program);
    assert!(
        block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::Int8(-8),
                ..
            }
        )),
        "expected `Const Int8(-8)` field init (negated-literal fold), got {:?}",
        block.instructions,
    );
    // The fold is the whole point: no separate `UnaryOp::Neg`
    // instruction should remain — a single typed `Const` at the
    // recorded width replaces the `Const(8)` + `Neg` pair.
    assert!(
        !block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::UnaryOp {
                op: IRUnaryOp::Neg,
                ..
            }
        )),
        "expected `-8` to fold into a single Int8 const, found a runtime UnaryOp::Neg in {:?}",
        block.instructions,
    );
}

#[test]
fn return_type_uint16_coerces_literal_to_const_uint16() {
    let program = lower("fn answer -> UInt16\n  65_535\nend\n\nfn main\n  answer()\nend\n");
    let answer = program
        .function(&format!("{}.answer", common::PACKAGE))
        .expect("missing `answer` function in lowered program");
    let block = answer
        .blocks
        .first()
        .expect("`answer` should have an entry block");
    assert!(
        block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::UInt16(65_535),
                ..
            }
        )),
        "expected `Const UInt16(65535)` return, got {:?}",
        block.instructions,
    );
}

#[test]
fn return_type_float32_coerces_literal_to_const_float32() {
    let program = lower("fn half -> Float32\n  0.5\nend\n\nfn main\n  half()\nend\n");
    let half = program
        .function(&format!("{}.half", common::PACKAGE))
        .expect("missing `half` function in lowered program");
    let block = half
        .blocks
        .first()
        .expect("`half` should have an entry block");
    assert!(
        block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::Float32(v),
                ..
            } if *v == 0.5_f32,
        )),
        "expected `Const Float32(0.5)` return, got {:?}",
        block.instructions,
    );
}

#[test]
fn negated_literal_in_uncoerced_position_keeps_runtime_neg() {
    // Without a narrow-target site, `-7` still lowers to the
    // pre-coercion shape: `Const Int64(7)` + `UnaryOp::Neg`. Pins
    // that the fold only fires when a coercion record is present.
    let program = lower("fn main\n  -7\nend\n");
    let block = entry_block(&program);
    assert!(
        block.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::UnaryOp {
                op: IRUnaryOp::Neg,
                ..
            }
        )),
        "expected runtime UnaryOp::Neg with no coercion, got {:?}",
        block.instructions,
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
