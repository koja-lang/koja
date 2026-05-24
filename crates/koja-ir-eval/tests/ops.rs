//! End-to-end coverage for the operator math in `src/ops.rs`:
//! `apply_binary_op` (`and`, `or`, `==`, `!=`, `<`, `>`, `<=`, `>=`)
//! and `apply_unary_op` (`not`, unary `-`).
//!
//! Eager semantics: both sides always evaluated; result produced by
//! a single `BinaryOp` / `UnaryOp` instruction. Source-driven (parse
//! → check → lower → run) so the tests stay faithful to the IR
//! shape lowering produces; the helpers themselves never see anything
//! but pre-resolved [`Value`] operands.

use koja_ir_eval::Value;

mod common;

use common::{evaluate_program as evaluate, evaluate_script};

#[test]
fn logical_and_evaluates_eagerly() {
    assert_eq!(
        evaluate("fn main -> Bool\n  true and false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  true and true\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn logical_or_evaluates_eagerly() {
    assert_eq!(
        evaluate("fn main -> Bool\n  false or false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  true or false\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn not_flips_its_operand() {
    assert_eq!(
        evaluate("fn main -> Bool\n  not false\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  not true\nend\n").unwrap(),
        Value::Bool(false),
    );
}

#[test]
fn neg_flips_int_sign() {
    assert_eq!(
        evaluate("fn main -> Int\n  -7\nend\n").unwrap(),
        Value::Int(-7),
    );
    assert_eq!(
        evaluate("fn main -> Int\n  -(3 - 5)\nend\n").unwrap(),
        Value::Int(2),
    );
}

#[test]
fn string_concat_appends_payloads() {
    // End-to-end String <> String through the interpreter — pins
    // `concat_values`'s String arm. Phase C will add Binary/Bits
    // coverage once `<<…>>` literals can mint those values from
    // source.
    assert_eq!(
        evaluate("fn main -> String\n  \"foo\" <> \"bar\"\nend\n").unwrap(),
        Value::String("foobar".into()),
    );
}

#[test]
fn integer_comparisons_produce_bool() {
    assert_eq!(
        evaluate("fn main -> Bool\n  1 < 2\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  2 < 1\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  1 == 1\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  1 != 1\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  3 >= 3\nend\n").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  3 > 3\nend\n").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  2 <= 3\nend\n").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn bool_equality_produces_bool() {
    assert_eq!(
        evaluate("fn main -> Bool\n  true == false\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  true != false\nend\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn composed_expression_evaluates_correctly() {
    assert_eq!(
        evaluate("fn main -> Bool\n  (1 == 1) and (2 != 3)\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  (1 < 2) or (3 > 100)\nend\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  not (1 == 2)\nend\n").unwrap(),
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

#[test]
fn float_arithmetic_evaluates_natively() {
    assert_eq!(
        evaluate("fn main -> Float64\n  2.0 + 2.0\nend\n").unwrap(),
        Value::Float64(4.0),
    );
    assert_eq!(
        evaluate("fn main -> Float64\n  3.5 - 1.25\nend\n").unwrap(),
        Value::Float64(2.25),
    );
    assert_eq!(
        evaluate("fn main -> Float64\n  1.5 * 4.0\nend\n").unwrap(),
        Value::Float64(6.0),
    );
}

#[test]
fn float_division_by_zero_returns_inf() {
    let value = evaluate("fn main -> Float64\n  1.0 / 0.0\nend\n").unwrap();
    let Value::Float64(v) = value else {
        panic!("expected Float64, got {value:?}");
    };
    assert!(v.is_infinite() && v.is_sign_positive());
}

#[test]
fn nan_comparisons_return_false() {
    // `0.0 / 0.0` is `NaN`; every ordered predicate against `NaN`
    // must return `false` (matches IEEE 754 + LLVM `OEQ`/`OLT`).
    assert_eq!(
        evaluate("fn main -> Bool\n  (0.0 / 0.0) == (0.0 / 0.0)\nend\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate("fn main -> Bool\n  (0.0 / 0.0) < 1.0\nend\n").unwrap(),
        Value::Bool(false),
    );
}

#[test]
fn unary_float_neg_flips_sign() {
    assert_eq!(
        evaluate("fn main -> Float64\n  -2.5\nend\n").unwrap(),
        Value::Float64(-2.5),
    );
    assert_eq!(
        evaluate("fn main -> Float64\n  -(1.0 - 4.0)\nend\n").unwrap(),
        Value::Float64(3.0),
    );
}
