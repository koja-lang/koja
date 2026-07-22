//! Numeric-literal coercion at the six type-equality sites plus
//! const initializers. A literal flowing into a sized target
//! coerces when its compile-time value fits the target's range.
//! Out-of-range / sign-mismatch cases produce precise narrow-int
//! diagnostics. Non-literal sources still type-check strictly.

use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

#[test]
fn positive_int_literal_fits_call_arg_uint8() {
    let source = "
        fn take(x: UInt8) -> Unit
          ()
        end

        take(255)
        ";
    typecheck(&dedent(source));
}

#[test]
fn negative_int_literal_fits_call_arg_int8() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        take(-128)
        ";
    typecheck(&dedent(source));
}

#[test]
fn negative_int_literal_into_uint8_diagnoses_out_of_range() {
    let source = "
        fn take(x: UInt8) -> Unit
          ()
        end

        take(-1)
        ";
    assert_script_fails_with(source, &["-1", "UInt8", "0..=255"]);
}

#[test]
fn hex_int_literal_fits_call_arg_uint8() {
    let source = "
        fn take(x: UInt8) -> Unit
          ()
        end

        take(0xFF)
        ";
    typecheck(&dedent(source));
}

#[test]
fn decimal_overflow_into_int8_diagnoses_out_of_range() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        take(128)
        ";
    assert_script_fails_with(source, &["128", "Int8", "-128..=127"]);
}

#[test]
fn struct_field_uint8_accepts_fitting_literal() {
    let source = "
        struct Fd
          descriptor: UInt8
        end

        Fd{descriptor: 1}
        ";
    typecheck(&dedent(source));
}

#[test]
fn struct_field_uint8_rejects_overflow_literal() {
    let source = "
        struct Fd
          descriptor: UInt8
        end

        Fd{descriptor: 256}
        ";
    assert_script_fails_with(source, &["descriptor", "256", "UInt8"]);
}

#[test]
fn enum_tuple_payload_int16_accepts_fitting_literal() {
    let source = "
        enum Signal
          Tone(Int16)
        end

        Signal.Tone(32_767)
        ";
    typecheck(&dedent(source));
}

#[test]
fn enum_tuple_payload_int16_rejects_overflow_literal() {
    let source = "
        enum Signal
          Tone(Int16)
        end

        Signal.Tone(40_000)
        ";
    assert_script_fails_with(source, &["Tone", "40000", "Int16"]);
}

#[test]
fn return_type_int8_accepts_fitting_literal() {
    let source = "
        fn answer -> Int8
          42
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn return_type_int8_rejects_overflow_literal() {
    let source = "
        fn answer -> Int8
          200
        end
        ";
    assert_script_fails_with(source, &["answer", "200", "Int8"]);
}

#[test]
fn const_initializer_uint8_accepts_fitting_literal() {
    let source = "
        const STDOUT: UInt8 = 1

        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn const_initializer_uint8_rejects_negative_literal() {
    let source = "
        const BAD: UInt8 = -1

        ()
        ";
    assert_script_fails_with(source, &["-1", "UInt8"]);
}

#[test]
fn alias_int_accepts_negative_literal() {
    let source = "
        fn take(x: Int) -> Unit
          ()
        end

        take(-1)
        ";
    typecheck(&dedent(source));
}

#[test]
fn alias_float_accepts_arbitrary_value() {
    let source = "
        fn take(x: Float) -> Unit
          ()
        end

        take(3.14)
        ";
    typecheck(&dedent(source));
}

#[test]
fn float32_accepts_round_trip_safe_literal() {
    let source = "
        fn take(x: Float32) -> Unit
          ()
        end

        take(0.5)
        ";
    typecheck(&dedent(source));
}

#[test]
fn float32_rejects_non_representable_literal() {
    let source = "
        fn take(x: Float32) -> Unit
          ()
        end

        take(0.1)
        ";
    assert_script_fails_with(source, &["Float32", "0.1"]);
}

#[test]
fn non_numeric_source_keeps_strict_type_mismatch() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        take(\"hello\")
        ";
    assert_script_fails_with(source, &["expects", "Int8", "String"]);
}

#[test]
fn pattern_literal_uint8_fits_subject_uint8() {
    let source = "
        fn classify(x: UInt8) -> Int
          match x
            5 -> 1
            _ -> 0
          end
        end

        classify(5)
        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn pattern_literal_uint8_out_of_range_diagnoses() {
    let source = "
        fn classify(x: UInt8) -> Int
          match x
            300 -> 1
            _ -> 0
          end
        end

        classify(5)
        ()
        ";
    assert_script_fails_with(source, &["300", "UInt8", "0..=255"]);
}

#[test]
fn pattern_literal_int8_negative_fits() {
    let source = "
        fn classify(x: Int8) -> Int
          match x
            -128 -> 1
            _ -> 0
          end
        end

        classify(-1)
        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_eq_int8_with_int_literal_fits() {
    let source = "
        fn check(x: Int8) -> Bool
          x == 0
        end

        check(1)
        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_gte_int32_with_int_literal_fits() {
    let source = "
        fn nonneg(fd: Int32) -> Bool
          fd >= 0
        end

        nonneg(1)
        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_eq_uint8_with_int_literal_out_of_range_diagnoses() {
    let source = "
        fn check(x: UInt8) -> Bool
          x == 256
        end

        check(1)
        ()
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Bool") || (m.contains("UInt8") && m.contains("Int"))),
        "expected mismatch diagnostic for out-of-range literal at comparison site, \
         got {messages:?}",
    );
}

#[test]
fn binary_gt_float32_with_float_literal_fits() {
    let source = "
        fn check(x: Float32) -> Bool
          x > 1.5
        end

        check(2.0)
        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_gte_int64_with_int_literal_aliases() {
    let source = "
        @extern \"C\"
        fn ext_returns_int64() -> Int64

        fn check() -> Bool
          result = ext_returns_int64()
          result >= 0
        end

        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn result_ok_payload_int64_unifies_with_expected_int() {
    // Function declares `Result<Int, String>`, payload binding T sees
    // `Int64`. The bidirectional fill must find `E = String` even
    // though `T` was bound to `Int64` and the template says `Int`.
    let source = "
        @extern \"C\"
        fn ext_returns_int64() -> Int64

        fn write() -> Result<Int, String>
          result = ext_returns_int64()
          result >= 0 ? Result.Ok(result) : Result.Err(\"failed\")
        end

        ()
        ";
    typecheck(&dedent(source));
}

#[test]
fn tuple_literal_elements_fit_contextual_numeric_widths() {
    let source = "
        type Sample = (UInt8, (Int8, Float32))

        sample: Sample = (255, (-128, 0.5))
        sample
        ";

    typecheck(&dedent(source));
}

#[test]
fn tuple_literal_element_out_of_range_diagnoses() {
    let source = "
        fn take(sample: (UInt8, String)) -> Unit
          ()
        end

        take((256, \"too wide\"))
        ";

    assert_script_fails_with(source, &["256", "UInt8", "0..=255"]);
}
