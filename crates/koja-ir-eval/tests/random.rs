//! Eval coverage for the auto-imported `Global.random` stdlib file.
//! `Random.bytes` adopts the runtime's managed Binary block:
//!
//! - `koja_random_bytes` routes through the curated extern table
//!   into `koja-runtime`'s `koja_random_bytes`, so eval consumes
//!   the same OS entropy as the LLVM backend.
//! - `RuntimeBlock.adopt_binary` reads the payload, frees the source
//!   runtime block, and returns a `Value::Binary`.
//!
//! `Random.int` is a direct extern shim. We pin both ends with
//! `min == max` (deterministic) so the test doesn't depend on the
//! live entropy source.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

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
