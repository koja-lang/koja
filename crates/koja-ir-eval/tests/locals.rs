//! Runtime coverage for local slots ([`IRInstruction::LocalDecl`] /
//! [`IRInstruction::LocalRead`] / [`IRInstruction::LocalWrite`]):
//! declaration and read, reassignment, param promotion, and frame
//! isolation across nested calls.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
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
fn param_reference_threads_arg_to_body() {
    let source = "
        fn id(n: Int) -> Int
          n
        end

        id(42)
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(42));
}

#[test]
fn param_reassignment_replaces_slot_in_callee() {
    let source = "
        fn shadow(n: Int) -> Int
          n = n + 1
          n
        end

        shadow(10)
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(11));
}

#[test]
fn nested_call_does_not_leak_callee_local_into_caller_frame() {
    // Each function gets its own `Frame`. `caller`'s `x` and
    // `helper`'s `n` are different slots. If frames bled together
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

        caller()
        ";

    let value = evaluate_script(&dedent(source));
    assert_eq!(value, Value::Int(1));
}

#[test]
fn local_assignment_inside_if_arm_is_visible_after_arm() {
    // Locals are script-body-scoped (no block scoping yet),
    // so an assignment inside an `if` arm reaches the trailing
    // expression in the same body.
    let source = "
        x = 0
        if true
          x = 7
        end
        x
        ";

    let value = evaluate_script(&dedent(source));
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
