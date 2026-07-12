//! End-to-end coverage for operators: short-circuit `and` / `or`,
//! `apply_binary_op` (`==`, `!=`, `<`, `>`, `<=`, `>=`), and
//! `apply_unary_op` (`not`, unary `-`).
//!
//! Source-driven (parse -> check -> lower -> run) so the tests stay
//! faithful to the control flow and IR instruction shapes lowering
//! produces.

use koja_ir_eval::{RuntimeError, Value};

mod common;

use common::evaluate_script;

#[test]
fn logical_and_returns_bool() {
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
fn logical_or_returns_bool() {
    assert_eq!(
        evaluate_script("false or false\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate_script("true or false\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn logical_rhs_panics_are_skipped() {
    let setup = "items: List<Int> = []\n";
    assert_eq!(
        evaluate_script(&format!("{setup}false and items.get(0).unwrap() == 1\n")).unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate_script(&format!("{setup}true or items.get(0).unwrap() == 1\n")).unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn not_flips_its_operand() {
    assert_eq!(evaluate_script("not false\n").unwrap(), Value::Bool(true),);
    assert_eq!(evaluate_script("not true\n").unwrap(), Value::Bool(false),);
}

#[test]
fn neg_flips_int_sign() {
    assert_eq!(evaluate_script("-7\n").unwrap(), Value::Int(-7),);
    assert_eq!(evaluate_script("-(3 - 5)\n").unwrap(), Value::Int(2),);
}

#[test]
fn string_concat_appends_payloads() {
    // End-to-end String <> String through the interpreter — pins
    // `concat_values`'s String arm. Phase C will add Binary/Bits
    // coverage once `<<…>>` literals can mint those values from
    // source.
    assert_eq!(
        evaluate_script("\"foo\" <> \"bar\"\n").unwrap(),
        Value::string("foobar"),
    );
}

#[test]
fn integer_comparisons_produce_bool() {
    assert_eq!(evaluate_script("1 < 2\n").unwrap(), Value::Bool(true));
    assert_eq!(evaluate_script("2 < 1\n").unwrap(), Value::Bool(false));
    assert_eq!(evaluate_script("1 == 1\n").unwrap(), Value::Bool(true));
    assert_eq!(evaluate_script("1 != 1\n").unwrap(), Value::Bool(false));
    assert_eq!(evaluate_script("3 >= 3\n").unwrap(), Value::Bool(true));
    assert_eq!(evaluate_script("3 > 3\n").unwrap(), Value::Bool(false));
    assert_eq!(evaluate_script("2 <= 3\n").unwrap(), Value::Bool(true));
}

#[test]
fn bool_equality_produces_bool() {
    assert_eq!(
        evaluate_script("true == false\n").unwrap(),
        Value::Bool(false),
    );
    assert_eq!(
        evaluate_script("true != false\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn composed_expression_evaluates_correctly() {
    assert_eq!(
        evaluate_script("(1 == 1) and (2 != 3)\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate_script("(1 < 2) or (3 > 100)\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate_script("not (1 == 2)\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn float_arithmetic_evaluates_natively() {
    assert_eq!(evaluate_script("2.0 + 2.0\n").unwrap(), Value::Float64(4.0),);
    assert_eq!(
        evaluate_script("3.5 - 1.25\n").unwrap(),
        Value::Float64(2.25),
    );
    assert_eq!(evaluate_script("1.5 * 4.0\n").unwrap(), Value::Float64(6.0),);
}

#[test]
fn float_division_by_zero_panics() {
    // `1.0 / 0.0` would be `+inf` under raw IEEE; the finite-only
    // `Float` invariant traps it instead.
    let error = evaluate_script("1.0 / 0.0\n").expect_err("non-finite result must trap");
    assert_eq!(
        error,
        RuntimeError::Panicked {
            message: "non-finite float result in /".to_string(),
        },
    );
}

#[test]
fn float_nan_producing_division_panics() {
    // `0.0 / 0.0` would be `NaN`; with the finite-only invariant it
    // traps, so NaN is unrepresentable in Koja.
    let error = evaluate_script("0.0 / 0.0\n").expect_err("NaN result must trap");
    assert_eq!(
        error,
        RuntimeError::Panicked {
            message: "non-finite float result in /".to_string(),
        },
    );
}

#[test]
fn unary_float_neg_flips_sign() {
    assert_eq!(evaluate_script("-2.5\n").unwrap(), Value::Float64(-2.5),);
    assert_eq!(
        evaluate_script("-(1.0 - 4.0)\n").unwrap(),
        Value::Float64(3.0),
    );
}

#[test]
fn float32_arithmetic_stays_at_f32_width() {
    // Same-width `Float32` arithmetic produces a `Float32` (regression:
    // the dispatch used to peek only `Float64` and routed these to the
    // int path, dying with a type mismatch).
    assert_eq!(
        evaluate_script("a: Float32 = 1.5\nb: Float32 = 2.5\na + b\n").unwrap(),
        Value::Float32(4.0),
    );
    assert_eq!(
        evaluate_script("a: Float32 = 1.5\nb: Float32 = 4.0\na * b\n").unwrap(),
        Value::Float32(6.0),
    );
}

#[test]
fn float32_arithmetic_rounds_at_f32_precision() {
    // 2^24 + 1 is not representable in f32 and rounds (half-to-even)
    // back to 2^24 — proof the math runs at f32 width, not widened to
    // f64 and cast.
    assert_eq!(
        evaluate_script("a: Float32 = 16777216.0\nb: Float32 = 1.0\na + b\n").unwrap(),
        Value::Float32(16777216.0),
    );
}

#[test]
fn float32_comparisons_produce_bool() {
    assert_eq!(
        evaluate_script("a: Float32 = 1.5\nb: Float32 = 2.5\na < b\n").unwrap(),
        Value::Bool(true),
    );
    assert_eq!(
        evaluate_script("a: Float32 = 2.5\nb: Float32 = 2.5\na >= b\n").unwrap(),
        Value::Bool(true),
    );
}

#[test]
fn unary_float32_neg_flips_sign() {
    assert_eq!(
        evaluate_script("a: Float32 = 2.5\n-a\n").unwrap(),
        Value::Float32(-2.5),
    );
}
