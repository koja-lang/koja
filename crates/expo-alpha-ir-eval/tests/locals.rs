//! Runtime coverage for the locals slice in
//! [`expo_alpha_ir_eval::Interpreter`]. The IR's local-slot
//! instructions ([`IRInstruction::LocalDecl`] /
//! [`IRInstruction::LocalRead`] / [`IRInstruction::LocalWrite`])
//! lower to per-frame storage in the interpreter; these tests pin
//! the observable behavior end-to-end:
//!
//! - Variable declaration + read returns the bound value.
//! - Reassignment overwrites the slot in place.
//! - Function parameters are reachable from the body via the same
//!   `LocalRead` path body-level locals use (param promotion).
//! - Nested calls keep their `Frame`s isolated (one function's
//!   slot doesn't leak into the caller's slot).

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

fn evaluate_program(source: &str) -> Value {
    common::evaluate_program(source).expect("interpreter should not error on this fixture")
}

#[test]
fn script_local_decl_then_read_returns_bound_value() {
    let source = "
        x = 7
        x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(7));
}

#[test]
fn script_local_reassignment_overwrites_slot() {
    let source = "
        x = 1
        x = 99
        x
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(99));
}

#[test]
fn program_param_reference_threads_arg_to_body() {
    let source = "
        fn id(n: Int) -> Int
          n
        end

        fn main -> Int
          id(42)
        end
        ";

    let value = evaluate_program(&dedent(source));
    assert_eq!(value, Value::Int(42));
}

#[test]
fn program_param_reassignment_replaces_slot_in_callee() {
    let source = "
        fn shadow(n: Int) -> Int
          n = n + 1
          n
        end

        fn main -> Int
          shadow(10)
        end
        ";

    let value = evaluate_program(&dedent(source));
    assert_eq!(value, Value::Int(11));
}

#[test]
fn nested_call_does_not_leak_callee_local_into_caller_frame() {
    // Each function gets its own `Frame`. `caller`'s `x` and
    // `helper`'s `n` are different slots; if frames bled together
    // we'd see `helper`'s value (5) instead of `caller`'s (1).
    let source = "
        fn helper(n: Int) -> Int
          n
        end

        fn caller -> Int
          x = 1
          helper(5)
          x
        end

        fn main -> Int
          caller()
        end
        ";

    let value = evaluate_program(&dedent(source));
    assert_eq!(value, Value::Int(1));
}

#[test]
fn local_assignment_inside_if_arm_is_visible_after_arm() {
    // Locals in alpha are function-scoped (no block scoping yet),
    // so an assignment inside an `if` arm reaches the trailing
    // expression in the same function.
    let source = "
        fn main -> Int
          x = 0
          if true
            x = 7
          end
          x
        end
        ";

    let value = evaluate_program(&dedent(source));
    assert_eq!(value, Value::Int(7));
}

#[test]
fn multiple_locals_each_get_their_own_slot() {
    let source = "
        a = 3
        b = 4
        a + b
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(7));
}
