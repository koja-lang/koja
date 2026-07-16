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

// ------------------------------------------------------------------
// Call-arg site (the `f(arg)` flavor of `validate_arg_signature`).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Struct-field site (`validate_named_fields`).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Enum tuple-payload site (`validate_tuple_payload`).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Return-type site (`check_return_type`).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Const initializer site (`lift_signatures::resolve_constant_value`).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// `Int` / `Float` aliases stay loose: any literal flows through, and
// negative literals into `Int` are still fine (it's the loosest
// integer type).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Float round-trip representability: `Float32` accepts a literal iff
// `f64 -> f32 -> f64` round-trips equal. `0.5` does, `0.1` does not
// (the closest f32 differs from the parsed f64).
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Sign mismatch: a `String` flowing into `Int8` still produces the
// pre-existing type-mismatch diagnostic, not the narrow-int range
// one. Coercion only kicks in for numeric literal sources.
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Pattern-literal site (`patterns/literals.rs::check_literal_matches_subject`).
// A literal pattern matched against a sized-numeric subject coerces
// when its value fits the subject's range. Out-of-range literals
// produce a precise narrow-int diagnostic instead of the generic
// "type does not match subject type" one.
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Binary-op comparison site (`ops::numeric_comparison_compatible`).
// A sized-numeric operand paired with a fitting `Int` / `Float`
// literal stamps the matching coercion on the literal so IR lower
// mints the narrow `Const` opcode at the comparison's bit width.
// Mirrors the pattern-literal arm above: same `check_compatible` /
// `coercion_target_mut` plumbing, just invoked at one more site.
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// Comparison-site alias-mix: a sized-numeric operand against a value
// of the aliased default type should typecheck without coercion. This
// is the `Substitution::set` cascade case under the hood: `T` binds
// to `Int64` from the payload and the expected-type fill picks `Int`,
// which `types_equivalent` accepts as the same type.
// ------------------------------------------------------------------

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
