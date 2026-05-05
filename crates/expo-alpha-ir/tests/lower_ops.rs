//! Coverage for the operator + literal helpers in `src/lower/ops.rs`:
//! `lower_literal`, `lower_bin_op`, `lower_unary_op`, and the
//! `*_result_type` helpers (observed indirectly through the
//! produced `IRInstruction` shape).
//!
//! The eager-lowering contract: both operands first, then a single
//! `BinaryOp` / `UnaryOp` for the result. Float literal lowering
//! surfaces a feature-gap diagnostic (the `Literal::Float` arm is
//! the only currently-reachable feature gap in this module).

use std::path::PathBuf;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRScript,
    IRTerminator, IRUnaryOp, LowerError, ValueId, lower_program, lower_script,
};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_ops.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn lower_as_script(source: &str) -> IRScript {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_ops.expo"),
            source: source.to_string(),
        }],
        ParseMode::Script,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    lower_script(&checked).expect("script lowering should succeed")
}

fn lower_err(source: &str, entry: &str) -> LowerError {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("lower_ops.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry_id = Identifier::new(PACKAGE, vec![entry.to_string()]);
    lower_program(&checked, entry_id).expect_err("lowering should surface diagnostics")
}

fn expect_diagnostics(err: LowerError) -> Vec<String> {
    match err {
        LowerError::Diagnostics(d) => d.into_iter().map(|diag| diag.message).collect(),
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}

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
fn float_literal_in_body_surfaces_feature_gap_diagnostic() {
    let source = "
        fn main
          1.5
        end
        ";

    let program = dedent(source);
    let messages = expect_diagnostics(lower_err(&program, "main"));
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("Float literals"),
        "expected Float-literal diagnostic, got: {messages:?}",
    );
}
