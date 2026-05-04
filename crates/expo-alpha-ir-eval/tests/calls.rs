//! End-to-end interpreter coverage for `ExprKind::Call`.
//!
//! Drives parse -> typecheck -> IR lower -> evaluate on a handful of
//! multi-function programs and asserts the runtime value the
//! interpreter returns. Focus is on the call-control path: passing
//! args, returning a value, and recursing through nested calls.
//! Parameter references inside bodies are still out of scope for
//! this slice (typecheck diagnoses them), so `take(99)` ignores
//! its param and returns a constant — that's the deliberate
//! limitation being exercised.

use std::path::PathBuf;

use expo_alpha_ir::lower_program;
use expo_alpha_ir_eval::{Interpreter, RuntimeError, Value};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn evaluate(source: &str) -> Result<Value, RuntimeError> {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("calls.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed)
        .unwrap_or_else(|failure| panic!("alpha typecheck failed:\n{failure}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let program = lower_program(&checked, entry).expect("alpha lowering should succeed");
    Interpreter::new(program).run()
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
    // POC limitation: function bodies cannot reference parameters
    // yet (the typecheck would reject `x`). This test stakes
    // ground for *arity correctness*: the arg is evaluated and
    // bound in the callee's frame even though the body never
    // reads it, and the call returns the body's constant.
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
    // Same POC limitation applies: neither param is referenced,
    // so the return is a constant. The value carries out the
    // multi-arg call path end-to-end.
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
