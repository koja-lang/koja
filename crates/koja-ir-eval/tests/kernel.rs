//! Coverage for the eval-side intrinsic families wired from
//! auto-imported `kernel.koja`: `Equality.eq` (Bool + 8 int widths),
//! `Hash.hash` (Bool + 8 int widths), and `Kernel.panic`. The tests
//! drive the auto-imported `.eq` / `.hash` methods and the
//! `Kernel.panic(message)` call directly, so each family is
//! exercised through `parse -> check -> lower -> run` without the
//! handlers needing to be re-exported from the `koja-ir-eval`
//! public surface.
//!
//! Eval flattens every integer width to [`Value::Int(i64)`], so the
//! `Equality` / `Hash` int impls collapse to one spec per family.
//! The narrow-width cells stay observable via call-site coercion at
//! sized param slots, exactly the same shape `tests/bitwise.rs`
//! uses for the 48-cell `Bitwise` family.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

mod common;

use common::{evaluate_program, evaluate_script};

fn run_script(source: &str) -> Result<Value, RuntimeError> {
    evaluate_script(&dedent(source))
}

fn run_program(source: &str) -> Result<Value, RuntimeError> {
    evaluate_program(&dedent(source))
}

#[test]
fn bool_eq_true_when_operands_match() {
    assert_eq!(run_script("true.eq(true)").unwrap(), Value::Bool(true),);
}

#[test]
fn bool_eq_false_when_operands_differ() {
    assert_eq!(run_script("true.eq(false)").unwrap(), Value::Bool(false),);
}

#[test]
fn int_eq_true_when_operands_match() {
    assert_eq!(run_script("42.eq(42)").unwrap(), Value::Bool(true),);
}

#[test]
fn int_eq_false_when_operands_differ() {
    assert_eq!(run_script("42.eq(7)").unwrap(), Value::Bool(false),);
}

#[test]
fn uint8_eq_dispatches_through_narrow_impl() {
    let v = run_program(
        "
        fn eq_u8(x: UInt8, y: UInt8) -> Bool
          x.eq(y)
        end

        fn main -> Bool
          eq_u8(255, 255)
        end
        ",
    )
    .unwrap();
    assert_eq!(v, Value::Bool(true));
}

/// SplitMix64, mirrored from the eval handler so the test pins the
/// exact byte-for-byte spec.
fn splitmix64(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

#[test]
fn bool_hash_returns_splitmix64_of_bit() {
    let true_hash = splitmix64(1) as i64;
    let false_hash = splitmix64(0) as i64;
    assert_eq!(run_script("true.hash()").unwrap(), Value::Int(true_hash));
    assert_eq!(run_script("false.hash()").unwrap(), Value::Int(false_hash));
}

#[test]
fn int_hash_returns_splitmix64_of_value() {
    let expected = splitmix64(42) as i64;
    assert_eq!(run_script("42.hash()").unwrap(), Value::Int(expected));
}

#[test]
fn int_hash_distinct_inputs_produce_distinct_outputs() {
    let h1 = match run_script("1.hash()").unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    };
    let h2 = match run_script("2.hash()").unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    };
    assert_ne!(
        h1, h2,
        "SplitMix64 should map distinct inputs to distinct outputs",
    );
}

#[test]
fn kernel_panic_surfaces_runtime_error_with_message() {
    // `Kernel.panic` returns `Never`, which lowers to an
    // `IRTerminator::Unreachable` after the call so the body's
    // SSA flow stays well-formed. The dispatch table converts the
    // call into `RuntimeError::Panicked` carrying the message
    // unchanged. Pin the verbatim payload here.
    let err = run_program(
        "
        fn main
          Kernel.panic(\"boom\")
        end
        ",
    )
    .expect_err("Kernel.panic must surface as a runtime error");
    match err {
        RuntimeError::Panicked { message } => assert_eq!(message, "boom"),
        other => panic!("expected RuntimeError::Panicked, got {other:?}"),
    }
}
