//! Typecheck pins for hub-only implicit numeric widening.
//!
//! The rule: `Int8` / `Int16` / `Int32` / `UInt8` / `UInt16` /
//! `UInt32` widen implicitly to `Int`, and `Float32` widens to
//! `Float` — at every value-flow site (call args, struct fields,
//! enum payloads, returns, annotated bindings). Everything else
//! stays strict:
//!
//! - no narrowing (`Int -> Int32`),
//! - no `UInt64 -> Int` (doesn't fit),
//! - no sideways widening between sized types (`Int8 -> Int16`),
//! - no widening at binary operators,
//! - no widening during generic unification,
//! - no chaining with union widening (`Int32` into `Int | String`).

use koja_ast::util::dedent;

mod common;

use common::{diagnostic_messages, typecheck_script as typecheck, typecheck_script_fail};

// -----------------------------------------------------------------------------
// Accepts: every widenable source at every flow site
// -----------------------------------------------------------------------------

#[test]
fn all_six_int_sources_widen_to_int_at_call_args() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        a: Int8 = 1
        b: Int16 = 2
        c: Int32 = 3
        d: UInt8 = 4
        e: UInt16 = 5
        f: UInt32 = 6
        want_int(a)
        want_int(b)
        want_int(c)
        want_int(d)
        want_int(e)
        want_int(f)
        ";
    typecheck(&dedent(source));
}

#[test]
fn float32_widens_to_float_at_call_args() {
    let source = "
        fn want_float(x: Float) -> Float
          x
        end

        f: Float32 = 1.5
        want_float(f)
        ";
    typecheck(&dedent(source));
}

#[test]
fn sized_int_widens_into_struct_field() {
    let source = "
        struct Holder
          n: Int
        end

        small: Int32 = 42
        Holder{n: small}
        ";
    typecheck(&dedent(source));
}

#[test]
fn sized_int_widens_into_enum_tuple_payload() {
    let source = "
        enum Wrap
          Value(Int)
        end

        small: UInt16 = 7
        Wrap.Value(small)
        ";
    typecheck(&dedent(source));
}

#[test]
fn sized_int_widens_at_return_position() {
    let source = "
        fn promote(x: Int32) -> Int
          x
        end

        small: Int32 = -7
        promote(small)
        ";
    typecheck(&dedent(source));
}

#[test]
fn sized_int_widens_at_annotated_binding() {
    let source = "
        small: Int8 = -5
        wide: Int = small
        wide
        ";
    typecheck(&dedent(source));
}

#[test]
fn float32_widens_at_annotated_binding() {
    let source = "
        f: Float32 = 1.5
        wide: Float = f
        wide
        ";
    typecheck(&dedent(source));
}

// -----------------------------------------------------------------------------
// Rejections: narrowing, UInt64, sideways, operators, generics, unions
// -----------------------------------------------------------------------------

#[test]
fn int_does_not_narrow_to_int32() {
    let source = "
        fn want32(n: Int32) -> Int32
          n
        end

        wide: Int = 5
        want32(wide)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int32`, got `Int`")),
        "expected an Int -> Int32 mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn uint64_does_not_widen_to_int() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        u: UInt64 = 5
        want_int(u)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int`, got `UInt64`")),
        "expected a UInt64 -> Int mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn int8_does_not_widen_sideways_to_int16() {
    let source = "
        fn want16(n: Int16) -> Int16
          n
        end

        small: Int8 = 1
        want16(small)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int16`, got `Int8`")),
        "expected an Int8 -> Int16 mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn uint8_does_not_widen_sideways_to_uint16() {
    let source = "
        fn want16(n: UInt16) -> UInt16
          n
        end

        small: UInt8 = 1
        want16(small)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `UInt16`, got `UInt8`")),
        "expected a UInt8 -> UInt16 mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn uint32_does_not_cross_to_int32() {
    let source = "
        fn want32(n: Int32) -> Int32
          n
        end

        u: UInt32 = 1
        want32(u)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int32`, got `UInt32`")),
        "expected a UInt32 -> Int32 mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn float_does_not_narrow_to_float32() {
    let source = "
        fn want32(x: Float32) -> Float32
          x
        end

        wide: Float = 1.5
        want32(wide)
        ";
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Float32`, got `Float`")),
        "expected a Float -> Float32 mismatch diagnostic, got: {messages:?}",
    );
}

#[test]
fn binary_operators_do_not_promote() {
    let source = "
        small: Int32 = 1
        wide: Int = 2
        small + wide
        ";
    typecheck_script_fail(&dedent(source));
}

#[test]
fn generic_inference_binds_the_actual_sized_type() {
    // `identity(small)` infers `T = Int32` — no widen during
    // unification — so the result still flows into an `Int32` slot.
    let source = "
        fn identity<T>(x: T) -> T
          x
        end

        fn want32(n: Int32) -> Int32
          n
        end

        small: Int32 = 42
        want32(identity(small))
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_inference_does_not_widen_to_unify() {
    // First arg binds `T = Int32`; the `Int` second arg must not
    // narrow (and `T` must not re-widen) to make the call fit.
    let source = "
        fn same<T>(a: T, b: T) -> T
          a
        end

        small: Int32 = 1
        wide: Int = 2
        same(small, wide)
        ";
    typecheck_script_fail(&dedent(source));
}

#[test]
fn widening_does_not_chain_into_union_membership() {
    // `Int32` widens to `Int`, but not to `Int | String` — the
    // union arm only accepts exact members.
    let source = "
        fn want_either(v: Int | String) -> Int
          0
        end

        small: Int32 = 1
        want_either(small)
        ";
    typecheck_script_fail(&dedent(source));
}
