//! Numeric-literal coercion at the six type-equality sites plus
//! const initializers. A literal flowing into a sized target
//! coerces when its compile-time value fits the target's range;
//! out-of-range / sign-mismatch cases produce precise narrow-int
//! diagnostics; non-literal sources still type-check strictly.

use expo_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
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

        fn main -> Unit
          take(255)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn negative_int_literal_fits_call_arg_int8() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        fn main -> Unit
          take(-128)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn negative_int_literal_into_uint8_diagnoses_out_of_range() {
    let source = "
        fn take(x: UInt8) -> Unit
          ()
        end

        fn main -> Unit
          take(-1)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("-1") && m.contains("UInt8") && m.contains("0..=255")),
        "expected `-1` / `UInt8` / range diagnostic, got {messages:?}",
    );
}

#[test]
fn hex_int_literal_fits_call_arg_uint8() {
    let source = "
        fn take(x: UInt8) -> Unit
          ()
        end

        fn main -> Unit
          take(0xFF)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn decimal_overflow_into_int8_diagnoses_out_of_range() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        fn main -> Unit
          take(128)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("128") && m.contains("Int8") && m.contains("-128..=127")),
        "expected `128` / `Int8` / range diagnostic, got {messages:?}",
    );
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

        fn main
          Fd{descriptor: 1}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn struct_field_uint8_rejects_overflow_literal() {
    let source = "
        struct Fd
          descriptor: UInt8
        end

        fn main
          Fd{descriptor: 256}
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("descriptor") && m.contains("256") && m.contains("UInt8")),
        "expected struct-field range diagnostic, got {messages:?}",
    );
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

        fn main
          Signal.Tone(32_767)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn enum_tuple_payload_int16_rejects_overflow_literal() {
    let source = "
        enum Signal
          Tone(Int16)
        end

        fn main
          Signal.Tone(40_000)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Tone") && m.contains("40000") && m.contains("Int16")),
        "expected enum-payload range diagnostic, got {messages:?}",
    );
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
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("answer") && m.contains("200") && m.contains("Int8")),
        "expected return-type range diagnostic, got {messages:?}",
    );
}

// ------------------------------------------------------------------
// Const initializer site (`lift_signatures::resolve_constant_value`).
// ------------------------------------------------------------------

#[test]
fn const_initializer_uint8_accepts_fitting_literal() {
    let source = "
        const STDOUT: UInt8 = 1

        fn main -> Unit
          ()
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn const_initializer_uint8_rejects_negative_literal() {
    let source = "
        const BAD: UInt8 = -1

        fn main -> Unit
          ()
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("-1") && m.contains("UInt8")),
        "expected const-initializer range diagnostic, got {messages:?}",
    );
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

        fn main -> Unit
          take(-1)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn alias_float_accepts_arbitrary_value() {
    let source = "
        fn take(x: Float) -> Unit
          ()
        end

        fn main -> Unit
          take(3.14)
        end
        ";
    typecheck(&dedent(source));
}

// ------------------------------------------------------------------
// Float round-trip representability: `Float32` accepts a literal iff
// `f64 → f32 → f64` round-trips equal. `0.5` does; `0.1` does not
// (the closest f32 differs from the parsed f64).
// ------------------------------------------------------------------

#[test]
fn float32_accepts_round_trip_safe_literal() {
    let source = "
        fn take(x: Float32) -> Unit
          ()
        end

        fn main -> Unit
          take(0.5)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn float32_rejects_non_representable_literal() {
    let source = "
        fn take(x: Float32) -> Unit
          ()
        end

        fn main -> Unit
          take(0.1)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Float32") && m.contains("0.1")),
        "expected float-round-trip range diagnostic, got {messages:?}",
    );
}

// ------------------------------------------------------------------
// Sign mismatch: a `String` flowing into `Int8` still produces the
// pre-existing type-mismatch diagnostic, not the narrow-int range
// one — coercion only kicks in for numeric literal sources.
// ------------------------------------------------------------------

#[test]
fn non_numeric_source_keeps_strict_type_mismatch() {
    let source = "
        fn take(x: Int8) -> Unit
          ()
        end

        fn main -> Unit
          take(\"hello\")
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects") && m.contains("Int8") && m.contains("String")),
        "expected strict type-mismatch diagnostic, got {messages:?}",
    );
}

// ------------------------------------------------------------------
// Pattern-literal site (`patterns/literals.rs::check_literal_matches_subject`).
// A literal pattern matched against a sized-numeric subject coerces
// when its value fits the subject's range; out-of-range literals
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

        fn main -> Unit
          classify(5)
          ()
        end
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

        fn main -> Unit
          classify(5)
          ()
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("300") && m.contains("UInt8") && m.contains("0..=255")),
        "expected pattern-literal range diagnostic, got {messages:?}",
    );
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

        fn main -> Unit
          classify(-1)
          ()
        end
        ";
    typecheck(&dedent(source));
}
