//! Runtime coverage for hub-only numeric widening and the explicit
//! checked narrowing intrinsics under [`koja_ir_eval::Interpreter`].
//!
//! The headline case is the FFI motivation: a *negative* `Int32`
//! widened into an `Int` slot must stay negative (sign extension,
//! not zero extension — the BoringSSL error-code bug). Unsigned
//! sources zero-extend, `Float32` converts to `Float64`, and the
//! `Int.to_*` / `UInt64.to_int` conversions round-trip values and
//! reject out-of-range ones with `NumericConversionError.OutOfRange`.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(&dedent(source)).expect("interpreter should not error on this fixture")
}

fn expect_int(value: Value) -> i64 {
    match value {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    }
}

#[test]
fn negative_int32_widens_into_int_and_stays_negative() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        small: Int32 = -7
        want_int(small)
        ";
    assert_eq!(expect_int(evaluate_script(source)), -7);
}

#[test]
fn uint8_widens_into_int_with_zero_extension() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        byte: UInt8 = 255
        want_int(byte)
        ";
    assert_eq!(expect_int(evaluate_script(source)), 255);
}

#[test]
fn float32_widens_into_float() {
    let source = "
        fn want_float(x: Float) -> Float
          x
        end

        f: Float32 = 1.5
        want_float(f)
        ";
    let value = evaluate_script(source);
    let Value::Float64(v) = value else {
        panic!("expected Value::Float64, got {value:?}");
    };
    assert_eq!(v, 1.5);
}

#[test]
fn int_narrows_to_int8_when_in_range() {
    let source = "
        match 100.to_int8()
          Result.Ok(v) -> 1
          Result.Err(e) -> 0
        end
        ";
    assert_eq!(expect_int(evaluate_script(source)), 1);
}

#[test]
fn int_narrowing_out_of_range_yields_conversion_error() {
    let source = "
        match 300.to_int8()
          Result.Ok(v) -> 1
          Result.Err(e) -> 0
        end
        ";
    assert_eq!(expect_int(evaluate_script(source)), 0);
}

#[test]
fn narrowed_value_round_trips_through_widening() {
    // Narrow `Int -> Int32`, then let the result widen back into
    // an `Int` slot — the value is preserved end to end.
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        match (-1234).to_int32()
          Result.Ok(v) -> want_int(v)
          Result.Err(e) -> 0
        end
        ";
    assert_eq!(expect_int(evaluate_script(source)), -1234);
}

#[test]
fn negative_int_does_not_convert_to_uint64() {
    let source = "
        match (-1).to_uint64()
          Result.Ok(v) -> 1
          Result.Err(e) -> 0
        end
        ";
    assert_eq!(expect_int(evaluate_script(source)), 0);
}

#[test]
fn uint64_to_int_succeeds_for_small_values() {
    let source = "
        u: UInt64 = 10
        match u.to_int()
          Result.Ok(v) -> v
          Result.Err(e) -> 0 - 1
        end
        ";
    assert_eq!(expect_int(evaluate_script(source)), 10);
}

#[test]
fn float_to_float32_rounds_and_returns_directly() {
    let source = "
        (2.5).to_float32()
        ";
    let value = evaluate_script(source);
    let Value::Float32(v) = value else {
        panic!("expected Value::Float32, got {value:?}");
    };
    assert_eq!(v, 2.5);
}
