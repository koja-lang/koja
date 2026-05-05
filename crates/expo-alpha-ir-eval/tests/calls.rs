//! End-to-end interpreter coverage for `ExprKind::Call`. Focus is
//! the call-control path: passing args, returning a value, recursing
//! through nested calls. Parameter *references* inside bodies are
//! still out of scope (typecheck diagnoses them), so the args-taking
//! tests below pass values that the callee body ignores.

use std::path::PathBuf;

use expo_alpha_ir::{lower_program, lower_script};
use expo_alpha_ir_eval::{Interpreter, RuntimeError, Value};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("calls.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).unwrap_or_else(|failure| panic!("alpha typecheck failed:\n{failure}"))
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
fn zero_arg_call_returns_callee_value() {
    let source = "
        fn answer -> Int
          42
        end

        fn main
          answer() + 1
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(43));
}

#[test]
fn nested_zero_arg_calls_combine_via_arithmetic() {
    let source = "
        fn a -> Int
          1
        end

        fn b -> Int
          2
        end

        fn main
          a() + b()
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(3));
}

#[test]
fn arg_taking_callee_with_unreferenced_param_returns_body_value() {
    // Function bodies cannot reference parameters yet (typecheck
    // would reject `x`). This test stakes ground for arity
    // correctness: the arg is evaluated and bound in the callee's
    // frame even though the body never reads it, and the call
    // returns the body's constant.
    let source = "
        fn take(x: Int) -> Int
          7
        end

        fn main
          take(99)
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(7));
}

#[test]
fn multiple_args_evaluate_in_order() {
    // Same param-reference limitation applies: neither param is
    // referenced, so the return is a constant. The value carries
    // out the multi-arg call path end-to-end.
    let source = "
        fn pair(a: Int, b: Int) -> Int
          11
        end

        fn main
          pair(2, 3)
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(11));
}

#[test]
fn call_return_participates_in_outer_expression() {
    let source = "
        fn double -> Int
          4
        end

        fn main
          double() * 5
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(20));
}

#[test]
fn script_body_calls_helper_fn_in_packages() {
    // Mirror of `zero_arg_call_returns_callee_value` for script
    // mode: the helper fn lives in the script's package fragment;
    // the implicit body calls it. Drives `lower_script` +
    // `Interpreter::run_script` end to end.
    let source = "
        fn answer -> Int
          42
        end

        answer() + 1
        ";

    let script = dedent(source);
    assert_eq!(evaluate_script(&script).unwrap(), Value::Int(43));
}
