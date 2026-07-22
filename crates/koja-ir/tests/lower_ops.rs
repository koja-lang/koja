//! Coverage for operator lowering. Arithmetic / comparison operators
//! produce `BinaryOp`, unary operators produce `UnaryOp`, and logical
//! `and` / `or` produce short-circuit CFGs with a Bool merge parameter.

use koja_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRInstruction, IRScript, IRTerminator, IRType, IRUnaryOp,
    ValueId,
};

mod common;

use common::{
    all_instructions, block_labeled, entry_block, lower_script_source as lower, script_function,
};

/// Entry block of a script body that must lower to a single block.
fn sole_block(script: &IRScript) -> &IRBasicBlock {
    assert_eq!(
        script.blocks.len(),
        1,
        "script bodies lower to a single basic block",
    );
    entry_block(&script.blocks)
}

#[test]
fn and_lowers_to_short_circuit_cfg() {
    let script = lower("true and false\n");
    let entry = &script.blocks[0];
    let right = block_labeled(&script.blocks, "and_right");
    let merge = block_labeled(&script.blocks, "and_merge");
    let IRTerminator::CondBranch {
        cond,
        else_target,
        then_target,
    } = &entry.terminator
    else {
        panic!("and entry should end in CondBranch: {:?}", entry.terminator);
    };
    assert_eq!(then_target.block, right.id);
    assert!(then_target.args.is_empty());
    assert_eq!(else_target.block, merge.id);
    assert_eq!(else_target.args.len(), 1);
    assert!(matches!(
        entry.instructions.as_slice(),
        [
            IRInstruction::Const {
                dest: left,
                value: ConstValue::Bool(true),
            },
            IRInstruction::Const {
                dest: bypass,
                value: ConstValue::Bool(false),
            },
        ] if left == cond && *bypass == else_target.args[0]
    ));
    let IRTerminator::Branch(right_exit) = &right.terminator else {
        panic!(
            "and right block should branch to merge: {:?}",
            right.terminator
        );
    };
    assert_eq!(right_exit.block, merge.id);
    assert_eq!(right_exit.args.len(), 1);
    assert_eq!(merge.params.len(), 1);
    assert_eq!(merge.params[0].ty, IRType::Bool);
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge.params[0].dest),
        },
    );
    assert!(
        !all_instructions(&script.blocks)
            .any(|instruction| matches!(instruction, IRInstruction::BinaryOp { .. }))
    );
}

#[test]
fn or_lowers_to_short_circuit_cfg() {
    let script = lower("false or true\n");
    let entry = &script.blocks[0];
    let right = block_labeled(&script.blocks, "or_right");
    let merge = block_labeled(&script.blocks, "or_merge");
    let IRTerminator::CondBranch {
        cond,
        else_target,
        then_target,
    } = &entry.terminator
    else {
        panic!("or entry should end in CondBranch: {:?}", entry.terminator);
    };
    assert_eq!(else_target.block, right.id);
    assert!(else_target.args.is_empty());
    assert_eq!(then_target.block, merge.id);
    assert_eq!(then_target.args.len(), 1);
    assert!(matches!(
        entry.instructions.as_slice(),
        [
            IRInstruction::Const {
                dest: left,
                value: ConstValue::Bool(false),
            },
            IRInstruction::Const {
                dest: bypass,
                value: ConstValue::Bool(true),
            },
        ] if left == cond && *bypass == then_target.args[0]
    ));
    assert!(matches!(right.terminator, IRTerminator::Branch(_)));
    assert_eq!(merge.params.len(), 1);
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge.params[0].dest),
        },
    );
}

#[test]
fn not_lowers_to_unary_op_not() {
    let script = lower("not true\n");
    let block = sole_block(&script);
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
                operand_ty: IRType::Bool,
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
    let script = lower("-7\n");
    let block = sole_block(&script);
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
                operand_ty: IRType::Int64,
            },
        ],
    );
}

#[test]
fn comparisons_lower_to_matching_ir_bin_ops() {
    for (source, expected) in [
        ("1 == 2\n", IRBinOp::Eq),
        ("1 != 2\n", IRBinOp::NotEq),
        ("1 < 2\n", IRBinOp::Lt),
        ("1 > 2\n", IRBinOp::Gt),
        ("1 <= 2\n", IRBinOp::LtEq),
        ("1 >= 2\n", IRBinOp::GtEq),
    ] {
        let script = lower(source);
        let block = sole_block(&script);
        let Some(IRInstruction::BinaryOp { op, .. }) = block.instructions.last() else {
            panic!("expected trailing BinaryOp for source {source:?}");
        };
        assert_eq!(*op, expected, "source = {source:?}");
    }
}

#[test]
fn hex_int_literal_lowers_with_correct_radix() {
    // The lexer hands lower the raw text `0xFF` (prefix preserved).
    // `lower/ops.rs::parse_int_literal` strips `0x` and dispatches
    // to `i64::from_str_radix(_, 16)`.
    let script = lower("0xFF\n");
    let block = sole_block(&script);
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
    let script = lower("0b1010\n");
    let block = sole_block(&script);
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
    // `1_000_000` is decimal-with-underscores. The parser keeps the
    // underscores in the token text, so lower must strip them.
    let script = lower("1_000_000\n");
    let block = sole_block(&script);
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
    let script = lower("1.5\n");
    let block = sole_block(&script);
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
    let script = lower("2.0 + 2.0\n");
    let block = sole_block(&script);
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
                operand_ty: IRType::Float64,
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

// Narrow-int / narrow-float literal coercion. Literal values flowing
// into a sized target slot mint `Const` instructions at the recorded
// width rather than the default `Int64` / `Float64` head. The tests
// span every literal-fit site (call arg, struct field, return) so a
// regression at any one site fails a dedicated case.

#[test]
fn call_arg_uint8_coerces_literal_to_const_uint8() {
    let script = lower("fn take(x: UInt8) -> Unit\n  ()\nend\n\ntake(255)\n");
    let block = sole_block(&script);
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
    let script = lower("struct Sample\n  amplitude: Int8\nend\n\nSample{amplitude: -8}\n");
    let block = sole_block(&script);
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
    // The fold is the whole point. No separate `UnaryOp::Neg`
    // instruction should remain, a single typed `Const` at the
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
    let script = lower("fn answer -> UInt16\n  65_535\nend\n\nanswer()\n");
    let answer = script_function(&script, "answer");
    let block = entry_block(&answer.blocks);
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
    let script = lower("fn half -> Float32\n  0.5\nend\n\nhalf()\n");
    let half = script_function(&script, "half");
    let block = entry_block(&half.blocks);
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
fn tuple_literal_element_coerces_before_tuple_init() {
    let script = lower("fn sample -> (UInt8, String)\n  (255, \"narrow\")\nend\n\nsample()\n");
    let sample = script_function(&script, "sample");
    let block = entry_block(&sample.blocks);
    assert!(
        block.instructions.iter().any(|instruction| matches!(
            instruction,
            IRInstruction::Const {
                value: ConstValue::UInt8(255),
                ..
            }
        )),
        "expected tuple element to lower as `Const UInt8(255)`, got {:?}",
        block.instructions,
    );
    assert!(
        block.instructions.iter().any(|instruction| matches!(
            instruction,
            IRInstruction::TupleInit { ty, .. }
                if ty.as_slice() == [IRType::UInt8, IRType::String]
        )),
        "expected tuple to initialize with the coerced element type, got {:?}",
        block.instructions,
    );
}

#[test]
fn negated_literal_in_uncoerced_position_keeps_runtime_neg() {
    // Without a narrow-target site, `-7` still lowers to the
    // pre-coercion shape: `Const Int64(7)` + `UnaryOp::Neg`. Pins
    // that the fold only fires when a coercion record is present.
    let script = lower("-7\n");
    let block = sole_block(&script);
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
    let script = lower("1.0 < 2.0\n");
    let block = sole_block(&script);
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
