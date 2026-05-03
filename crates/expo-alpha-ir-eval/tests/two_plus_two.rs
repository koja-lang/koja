//! End-to-end smoke test for the alpha pipeline at its POC scope:
//!
//!   parse → expo-alpha-typecheck → expo-alpha-ir → expo-alpha-ir-eval
//!
//! Drives `fn main; 2 + 2; end` all the way through the alpha pipeline
//! and asserts the interpreter returns `Value::Int(4)`. When this
//! passes, the alpha pipeline has end-to-end coverage from source to
//! a runtime value.

use std::path::PathBuf;

use expo_alpha_ir::lower_program;
use expo_alpha_ir_eval::{Interpreter, RuntimeError, Value};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn evaluate(source: &str) -> Result<Value, RuntimeError> {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("two_plus_two.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).expect("alpha typecheck should succeed on POC source");
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let program = lower_program(&checked, entry).expect("alpha lowering should succeed");
    Interpreter::new(program).run()
}

#[test]
fn fn_main_two_plus_two_evaluates_to_int_four() {
    assert_eq!(evaluate("fn main\n  2 + 2\nend\n").unwrap(), Value::Int(4));
}

#[test]
fn integer_arithmetic_combinations() {
    assert_eq!(evaluate("fn main\n  10 - 3\nend\n").unwrap(), Value::Int(7));
    assert_eq!(evaluate("fn main\n  6 * 7\nend\n").unwrap(), Value::Int(42));
    assert_eq!(evaluate("fn main\n  20 / 4\nend\n").unwrap(), Value::Int(5));
    assert_eq!(evaluate("fn main\n  17 % 5\nend\n").unwrap(), Value::Int(2));
    assert_eq!(
        evaluate("fn main\n  (2 + 3) * 4\nend\n").unwrap(),
        Value::Int(20),
    );
}

#[test]
fn division_by_zero_surfaces_as_runtime_error() {
    let err = evaluate("fn main\n  10 / 0\nend\n").expect_err("should fail at runtime");
    assert!(matches!(err, RuntimeError::DivisionByZero { .. }));
}

#[test]
fn empty_main_returns_unit() {
    assert_eq!(evaluate("fn main\nend\n").unwrap(), Value::Unit);
}
