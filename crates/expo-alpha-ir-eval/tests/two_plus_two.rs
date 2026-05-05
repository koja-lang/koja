//! End-to-end smoke tests for integer arithmetic through the alpha
//! pipeline (parse → typecheck → IR → interpret), exercising both
//! `lower_program` (project mode) and `lower_script` (script mode).

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
            path: PathBuf::from("two_plus_two.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).expect("alpha typecheck should succeed")
}

fn evaluate_program(source: &str) -> Result<Value, RuntimeError> {
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
fn fn_main_two_plus_two_evaluates_to_int_four() {
    assert_eq!(
        evaluate_program("fn main\n  2 + 2\nend\n").unwrap(),
        Value::Int(4),
    );
}

#[test]
fn bare_two_plus_two_script_evaluates_to_int_four() {
    assert_eq!(evaluate_script("2 + 2\n").unwrap(), Value::Int(4));
}

#[test]
fn integer_arithmetic_combinations() {
    assert_eq!(
        evaluate_program("fn main\n  10 - 3\nend\n").unwrap(),
        Value::Int(7),
    );
    assert_eq!(
        evaluate_program("fn main\n  6 * 7\nend\n").unwrap(),
        Value::Int(42),
    );
    assert_eq!(
        evaluate_program("fn main\n  20 / 4\nend\n").unwrap(),
        Value::Int(5),
    );
    assert_eq!(
        evaluate_program("fn main\n  17 % 5\nend\n").unwrap(),
        Value::Int(2),
    );
    assert_eq!(
        evaluate_program("fn main\n  (2 + 3) * 4\nend\n").unwrap(),
        Value::Int(20),
    );
}

#[test]
fn script_mode_arithmetic_matches_project_mode() {
    assert_eq!(evaluate_script("10 - 3\n").unwrap(), Value::Int(7));
    assert_eq!(evaluate_script("(2 + 3) * 4\n").unwrap(), Value::Int(20));
}

#[test]
fn division_by_zero_surfaces_as_runtime_error() {
    let err = evaluate_program("fn main\n  10 / 0\nend\n").expect_err("should fail at runtime");
    assert!(matches!(err, RuntimeError::DivisionByZero { .. }));
}

#[test]
fn empty_main_returns_unit() {
    assert_eq!(evaluate_program("fn main\nend\n").unwrap(), Value::Unit,);
}
