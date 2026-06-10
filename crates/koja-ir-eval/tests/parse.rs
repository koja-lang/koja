//! Eval coverage for
//! `Int.parse(input: String) -> Result<Int, NumericConversionError>`
//! and `Float.parse(input: String) -> Result<Float, NumericConversionError>`
//! (the pure-Rust shims surfaced through `koja-ir-eval`'s
//! `intrinsics/parse.rs`). The Result enum is materialized directly
//! off `function.return_type`; `Ok(value)` lands as a tuple-payload
//! enum variant, `Err(_)` carries a `NumericConversionError` value
//! over the same enum — `InvalidFormat` for malformed text,
//! `OutOfRange` for well-formed numbers that don't fit.

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
fn int_parse_rejects_non_numeric_input_as_invalid_format() {
    let outcome = evaluate_script(&dedent(
        r#"
        match Int.parse("not-an-int")
          Ok(_) -> 0
          Err(NumericConversionError.InvalidFormat) -> 1
          Err(_) -> 2
        end
        "#,
    ))
    .expect("Int.parse(\"not-an-int\") should evaluate to Err(InvalidFormat)");
    assert_eq!(outcome, Value::Int(1));
}

#[test]
fn int_parse_rejects_overflow_as_out_of_range() {
    let outcome = evaluate_script(&dedent(
        r#"
        match Int.parse("99999999999999999999")
          Ok(_) -> 0
          Err(NumericConversionError.OutOfRange) -> 1
          Err(_) -> 2
        end
        "#,
    ))
    .expect("overflowing Int.parse should evaluate to Err(OutOfRange)");
    assert_eq!(outcome, Value::Int(1));
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
fn float_parse_rejects_non_numeric_input_as_invalid_format() {
    let outcome = evaluate_script(&dedent(
        r#"
        match Float.parse("abc")
          Ok(_) -> 0
          Err(NumericConversionError.InvalidFormat) -> 1
          Err(_) -> 2
        end
        "#,
    ))
    .expect("Float.parse(\"abc\") should evaluate to Err(InvalidFormat)");
    assert_eq!(outcome, Value::Int(1));
}

#[test]
fn float_parse_rejects_overflow_as_out_of_range() {
    // `1e999` is well-formed but rounds to infinity — only finite
    // values parse.
    let outcome = evaluate_script(&dedent(
        r#"
        match Float.parse("1e999")
          Ok(_) -> 0
          Err(NumericConversionError.OutOfRange) -> 1
          Err(_) -> 2
        end
        "#,
    ))
    .expect("overflowing Float.parse should evaluate to Err(OutOfRange)");
    assert_eq!(outcome, Value::Int(1));
}

#[test]
fn float_parse_rejects_infinity_tokens_as_invalid_format() {
    // Rust's f64 parser accepts `inf` / `infinity` / `nan`, but Koja
    // has no literal syntax for them — they're malformed input here.
    let outcome = evaluate_script(&dedent(
        r#"
        match Float.parse("inf")
          Ok(_) -> 0
          Err(NumericConversionError.InvalidFormat) -> 1
          Err(_) -> 2
        end
        "#,
    ))
    .expect("Float.parse(\"inf\") should evaluate to Err(InvalidFormat)");
    assert_eq!(outcome, Value::Int(1));
}

#[test]
fn int_parse_err_payload_carries_the_conversion_error_enum() {
    // Pin the Err arm's tuple-payload shape: a single unit-variant
    // `NumericConversionError` value, not a string message.
    let outcome = evaluate_script(&dedent(r#"Int.parse("nope")"#))
        .expect("Int.parse(\"nope\") should evaluate even when it's Err");
    let Value::Enum { name, payload, .. } = outcome else {
        panic!("expected Result enum value, got {outcome:?}");
    };
    assert_eq!(name, "Err");
    let EnumPayload::Tuple(fields) = payload else {
        panic!("expected Err to carry a tuple payload, got {payload:?}");
    };
    let [
        Value::Enum {
            name: error_name,
            payload: error_payload,
            ..
        },
    ] = fields.as_slice()
    else {
        panic!("expected Err payload to be a single enum value; got {fields:?}");
    };
    assert_eq!(error_name, "InvalidFormat");
    assert!(
        matches!(error_payload, EnumPayload::Unit),
        "InvalidFormat should be a unit variant, got {error_payload:?}",
    );
}
