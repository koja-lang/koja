//! Eval coverage for the auto-imported `Global.random` stdlib file.
//! `Random.bytes` flows `expo_random_bytes -> CPtr<UInt8>.to_string ->
//! String.to_binary`; eval honors every step:
//!
//! - `expo_random_bytes` routes through the curated extern table
//!   into `expo-runtime`'s `expo_random_bytes`, so eval consumes
//!   the same OS entropy as the LLVM backend.
//! - `CPtr<UInt8>.to_string` reads the length-prefixed Expo string
//!   ABI from the raw pointer, copies the payload out, frees the
//!   source header chunk, and returns a `Value::String`.
//! - `String.to_binary` is a zero-cost widening that lands as
//!   `Value::Binary`.
//!
//! `Random.int` is a direct extern shim; we pin both ends with
//! `min == max` (deterministic) so the test doesn't depend on the
//! live entropy source.

use expo_ast::util::dedent;
use expo_ir_eval::Value;

mod common;

use common::evaluate_script;

#[test]
fn random_int_with_collapsed_range_returns_min() {
    let v = evaluate_script(&dedent("Random.int(7, 7)"))
        .expect("Random.int(7, 7) should evaluate via the runtime extern");
    assert_eq!(v, Value::Int(7));
}

#[test]
fn random_bytes_returns_binary_of_requested_length() {
    let v = evaluate_script(&dedent("Random.bytes(32).byte_size()"))
        .expect("Random.bytes(32).byte_size() should evaluate end-to-end");
    assert_eq!(
        v,
        Value::Int(32),
        "Random.bytes(32) should produce a 32-byte Binary",
    );
}

#[test]
fn random_bytes_zero_length_returns_empty_binary() {
    // The runtime allocates a zero-byte payload but the header still
    // carries `bit_length = 0`, so eval reads zero bytes and frees
    // cleanly. Pin the empty-byte case so the boundary doesn't drift.
    let v = evaluate_script(&dedent("Random.bytes(0).byte_size()"))
        .expect("Random.bytes(0).byte_size() should evaluate end-to-end");
    assert_eq!(v, Value::Int(0));
}

#[test]
fn random_int_within_range_stays_in_bounds() {
    // Pull a value from a real (non-collapsed) range and assert it
    // falls within the inclusive bounds. We don't assert a specific
    // value because the runtime consults OS entropy.
    let source = dedent(
        r#"
        v = Random.int(10, 20)
        v >= 10 and v <= 20
        "#,
    );
    let v = evaluate_script(&source).expect("Random.int over [10, 20] should evaluate");
    assert_eq!(
        v,
        Value::Bool(true),
        "Random.int(10, 20) should land in [10, 20]"
    );
}
