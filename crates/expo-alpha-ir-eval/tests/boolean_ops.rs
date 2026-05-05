//! End-to-end interpreter coverage for the boolean and comparison
//! operators: `and`, `or`, `not`, `== != < > <= >=`, and unary `-`.
//!
//! Mirrors the `two_plus_two.rs` pattern: source → typecheck → lower
//! → interpret, asserting the returned [`Value`] matches eager
//! semantics (both sides always evaluated; result produced by a
//! single `BinaryOp`/`UnaryOp`).

use std::path::PathBuf;

use expo_alpha_ir::{lower_program, lower_script};
use expo_alpha_ir_eval::{Interpreter, RuntimeError, Value};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("boolean_ops.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).expect("alpha typecheck should succeed")
}

fn evaluate(source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let program = lower_program(&checked, entry).expect("alpha lowering should succeed");
    Interpreter::run_program(program)
}

fn evaluate_script(source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck(source, ParseMode::Script);
    let script = lower_script(&checked).expect("alpha script lowering should succeed");
    Interpreter::run_script(script)
}

#[test]
fn logical_and_evaluates_eagerly() {
    assert_eq!(
        evaluate("fn main\n  true and false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main\n  true and true\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn logical_or_evaluates_eagerly() {
    assert_eq!(
        evaluate("fn main\n  false or false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main\n  true or false\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn not_flips_its_operand() {
    assert_eq!(
        evaluate("fn main\n  not false\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main\n  not true\nend\n").unwrap(),
        Value::Bool(false),
    );
}

#[test]
fn neg_flips_int_sign() {
    assert_eq!(evaluate("fn main\n  -7\nend\n").unwrap(), Value::Int(-7),);
    assert_eq!(
        evaluate("fn main\n  -(3 - 5)\nend\n").unwrap(),
        Value::Int(2),
    );
}

#[test]
fn integer_comparisons_produce_bool() {
    assert_eq!(
        evaluate("fn main\n  1 < 2\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main\n  2 < 1\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main\n  1 == 1\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main\n  1 != 1\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main\n  3 >= 3\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main\n  3 > 3\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main\n  2 <= 3\nend\n").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn bool_equality_produces_bool() {
    assert_eq!(
        evaluate("fn main\n  true == false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main\n  true != false\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn composed_expression_evaluates_correctly() {
    assert_eq!(
        evaluate("fn main\n  (1 == 1) and (2 != 3)\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main\n  (1 < 2) or (3 > 100)\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main\n  not (1 == 2)\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn script_mode_logical_and_evaluates_eagerly() {
    assert_eq!(
        evaluate_script("true and false\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate_script("true and true\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn script_mode_comparison_evaluates_to_bool() {
    assert_eq!(evaluate_script("1 < 2\n").unwrap(), Value::Bool(true));
    assert_eq!(evaluate_script("3 == 3\n").unwrap(), Value::Bool(true));
}
