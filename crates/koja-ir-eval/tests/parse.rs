//! Eval coverage for `Int.parse(input: String) -> Result<Int, String>`
//! and `Float.parse(input: String) -> Result<Float, String>` (the
//! pure-Rust shims surfaced through `koja-ir-eval`'s
//! `intrinsics/parse.rs`). The Result enum is materialized directly
//! off `function.return_type`; `Ok(value)` lands as a tuple-payload
//! enum variant, `Err(message)` carries a `Value::String` byte
//! payload over the same enum.

use koja_ast::util::dedent;
use koja_ir_eval::{EnumPayload, Value};

mod common;

use common::evaluate_script;

#[test]
fn int_parse_returns_ok_for_valid_int() {
    // Pull the `Ok(123)` value out of the Result enum payload so the
    // assertion pins the value, not just the variant shape.
    let outcome = evaluate_script(&dedent(
        r#"
        match Int.parse("123")
          Ok(v) -> v
          Err(_) -> -1
        end
        "#,
    ))
    .expect("Int.parse(\"123\") should evaluate to Ok(123)");
    assert_eq!(outcome, Value::Int(123));
}

#[test]
fn int_parse_returns_err_for_non_numeric_input() {
    // Same shape, but exercise the Err arm — pin the variant via the
    // boolean fall-through so we don't have to inspect Value::String
    // contents (the exact error message is part of the contract
    // between eval and Rust's `parse::<i64>`).
    let outcome = evaluate_script(&dedent(
        r#"
        match Int.parse("not-an-int")
          Ok(_) -> true
          Err(_) -> false
        end
        "#,
    ))
    .expect("Int.parse(\"not-an-int\") should evaluate to Err(...)");
    assert_eq!(outcome, Value::Bool(false));
}

#[test]
fn int_parse_trims_leading_and_trailing_whitespace() {
    let outcome = evaluate_script(&dedent(
        r#"
        match Int.parse("  42  ")
          Ok(v) -> v
          Err(_) -> -1
        end
        "#,
    ))
    .expect("Int.parse should trim whitespace");
    assert_eq!(outcome, Value::Int(42));
}

#[test]
fn float_parse_returns_ok_for_valid_float() {
    // `1.5` exactly representable in f64; avoids clippy's
    // `approx_constant` lint on near-Pi literals.
    let outcome = evaluate_script(&dedent(
        r#"
        match Float.parse("1.5")
          Ok(v) -> v
          Err(_) -> -1.0
        end
        "#,
    ))
    .expect("Float.parse(\"1.5\") should evaluate to Ok(1.5)");
    assert_eq!(outcome, Value::Float64(1.5));
}

#[test]
fn float_parse_returns_err_for_non_numeric_input() {
    let outcome = evaluate_script(&dedent(
        r#"
        match Float.parse("abc")
          Ok(_) -> true
          Err(_) -> false
        end
        "#,
    ))
    .expect("Float.parse(\"abc\") should evaluate to Err(...)");
    assert_eq!(outcome, Value::Bool(false));
}

#[test]
fn int_parse_err_payload_carries_a_string_message() {
    // Pin the Err arm's tuple-payload shape: a single `Value::String`
    // describing the failure. The message is implementation-defined;
    // we only assert it's non-empty so a future tweak to the wording
    // doesn't break this test.
    let outcome = evaluate_script(&dedent(r#"Int.parse("nope")"#))
        .expect("Int.parse(\"nope\") should evaluate even when it's Err");
    let Value::Enum { name, payload, .. } = outcome else {
        panic!("expected Result enum value, got {outcome:?}");
    };
    assert_eq!(name, "Err");
    let EnumPayload::Tuple(fields) = payload else {
        panic!("expected Err to carry a tuple payload, got {payload:?}");
    };
    let [Value::String(bytes)] = fields.as_slice() else {
        panic!("expected Err payload to be a single Value::String; got {fields:?}");
    };
    assert!(
        !bytes.is_empty(),
        "Err payload should describe the failure, got empty message",
    );
}
