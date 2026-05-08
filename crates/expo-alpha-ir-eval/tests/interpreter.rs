//! End-to-end coverage for the interpreter dispatcher in
//! `src/interpreter.rs`. Drives `parse → check → lower →
//! Interpreter::run_*` to observe the runtime [`Value`] each shape
//! produces.
//!
//! Three behavior areas live here, all hosted by the same
//! `execute_blocks` / `execute_function` / `execute_instruction` path:
//!
//! - Basic arithmetic + return-shape dispatch (project mode and
//!   script mode).
//! - `IRInstruction::Call` dispatch: zero-arg callee, multi-arg
//!   callee, nested calls in arithmetic, script-mode helper-fn
//!   resolution.
//! - `IRTerminator::CondBranch` dispatch: `if` and `unless` selecting
//!   between two arms at runtime, with helper functions whose `if` /
//!   `unless` cond is a literal Bool (identifier references inside
//!   bodies aren't resolved until the locals slice).
//!
//! Operator math (`apply_binary_op` / `apply_unary_op`) lives in
//! `tests/ops.rs`, paired with `src/ops.rs`.

use expo_alpha_ir_eval::{RuntimeError, Value};
use expo_ast::util::dedent;

mod common;

use common::{evaluate_program as evaluate, evaluate_script};

// -- basic dispatch -------------------------------------------------

#[test]
fn fn_main_two_plus_two_evaluates_to_int_four() {
    assert_eq!(evaluate("fn main\n  2 + 2\nend\n").unwrap(), Value::Int(4),);
}

#[test]
fn bare_two_plus_two_script_evaluates_to_int_four() {
    assert_eq!(evaluate_script("2 + 2\n").unwrap(), Value::Int(4));
}

#[test]
fn integer_arithmetic_combinations() {
    assert_eq!(evaluate("fn main\n  10 - 3\nend\n").unwrap(), Value::Int(7),);
    assert_eq!(evaluate("fn main\n  6 * 7\nend\n").unwrap(), Value::Int(42),);
    assert_eq!(evaluate("fn main\n  20 / 4\nend\n").unwrap(), Value::Int(5),);
    assert_eq!(evaluate("fn main\n  17 % 5\nend\n").unwrap(), Value::Int(2),);
    assert_eq!(
        evaluate("fn main\n  (2 + 3) * 4\nend\n").unwrap(),
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
    let err = evaluate("fn main\n  10 / 0\nend\n").expect_err("should fail at runtime");
    assert!(matches!(err, RuntimeError::DivisionByZero { .. }));
}

#[test]
fn empty_main_returns_unit() {
    assert_eq!(evaluate("fn main\nend\n").unwrap(), Value::Unit,);
}

// -- string literals -----------------------------------------------

#[test]
fn string_literal_evaluates_to_value_string() {
    let source = "fn main -> String\n  \"hello\"\nend\n";
    assert_eq!(
        evaluate(source).unwrap(),
        Value::String("hello".to_string()),
    );
}

#[test]
fn string_literal_in_script_mode_evaluates_to_value_string() {
    assert_eq!(
        evaluate_script("\"hello\"\n").unwrap(),
        Value::String("hello".to_string()),
    );
}

#[test]
fn empty_string_literal_evaluates_to_empty_value_string() {
    assert_eq!(
        evaluate("fn main -> String\n  \"\"\nend\n").unwrap(),
        Value::String(String::new()),
    );
}

#[test]
fn value_string_displays_without_quotes() {
    let value = Value::String("hello".to_string());
    assert_eq!(format!("{value}"), "hello");
}

// -- Call instruction dispatch -------------------------------------

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

// -- CondBranch terminator dispatch --------------------------------

#[test]
fn if_with_true_condition_executes_then_branch() {
    // The early `return 1` inside the `if true` body fires; the
    // merge block's trailing `2` is unreachable.
    let source = "
        fn pick -> Int
          if true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn if_with_false_condition_falls_through_to_merge() {
    // The cond evaluates to `false`, so the then-block is skipped
    // entirely; the trailing `2` in the merge block is the
    // function's return value.
    let source = "
        fn pick -> Int
          if false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn unless_with_false_condition_executes_body() {
    // `unless cond` runs the body when cond is `false`. The early
    // `return 1` therefore fires when the cond is the literal
    // `false`.
    let source = "
        fn pick -> Int
          unless false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn unless_with_true_condition_skips_body() {
    let source = "
        fn pick -> Int
          unless true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn if_drives_program_mode_through_helper_calls() {
    // Mirror of the script-mode coverage above, exercising the
    // project-mode entry path. Each helper exercises a different
    // arm of the same `if` shape and `main` sums them.
    let source = "
        fn pick_then -> Int
          if true
            return 1
          end
          2
        end

        fn pick_merge -> Int
          if false
            return 1
          end
          2
        end

        fn main
          pick_then() + pick_merge()
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(3));
}

// -- value-producing if/else (block params) -------------------------

#[test]
fn if_else_value_producing_then_arm_with_true_condition() {
    // The if/else lowers to a 4-block CFG with a typed BlockParam
    // on the merge; cond=true reaches merge with the then-arm's
    // value, which becomes the function's return.
    let source = "
        fn pick -> Int
          if true
            7
          else
            9
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn if_else_value_producing_else_arm_with_false_condition() {
    let source = "
        fn pick -> Int
          if false
            7
          else
            9
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(9));
}

#[test]
fn if_else_with_diverging_then_arm_produces_else_value() {
    // The then-arm diverges via `return`; only the else-arm reaches
    // merge, passing its tail value via the BlockParam.
    let source = "
        fn pick -> Int
          if false
            return 1
          else
            42
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(42));
}

// -- cond -----------------------------------------------------------

#[test]
fn cond_first_matching_arm_drives_return() {
    let source = "
        fn pick -> Int
          cond
            true -> 1
            false -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn cond_second_arm_runs_when_first_test_is_false() {
    let source = "
        fn pick -> Int
          cond
            false -> 1
            true -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn cond_else_runs_when_no_arm_matches() {
    let source = "
        fn pick -> Int
          cond
            false -> 1
            false -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(3));
}
