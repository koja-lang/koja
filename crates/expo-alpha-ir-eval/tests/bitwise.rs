//! Coverage for the eval-side `Bitwise` intrinsic family wired in
//! `src/intrinsics/bitwise.rs`. The auto-imported `Global.bitwise`
//! source brings every `band` / `bor` / `bxor` / `bnot` / `bsl` /
//! `bsr` method into scope, so test bodies just call them as
//! ordinary methods on integer literals.
//!
//! Eval flattens every integer width to [`Value::Int(i64)`], so
//! these tests double as the canonical specification of the
//! interpreter's bitwise semantics: AND / OR / XOR are
//! width-agnostic, NOT is `!lhs` on i64 (so `Int.bnot(0) = -1`),
//! left shift uses native `wrapping_shl`, and right shift branches
//! on the receiver type (`Int.bsr` arithmetic, `UInt*.bsr`
//! logical). The unsigned-shift divergence isn't asserted at the
//! eval layer today: every integer width collapses to i64 in
//! [`Value`] and the narrow widths only show up via literal
//! coercion at typed param / return slots, so `UInt*.bsr` and
//! `Int.bsr` agree on every value representable as a non-negative
//! i64. The divergence is pinned at the LLVM emitter layer
//! (`tests/intrinsics.rs`) where the emitted `lshr` vs `ashr` is
//! observable.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

use common::{evaluate_program, evaluate_script};

fn run_int(source: &str) -> i64 {
    match evaluate_script(&dedent(source)).unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    }
}

#[test]
fn int_band_returns_bitwise_and() {
    // `0b1100 & 0b1010 = 0b1000`; binary literals exercise the
    // alpha-IR `lower/ops.rs` radix-aware parser end-to-end.
    let v = run_int("0b1100.band(0b1010)");
    assert_eq!(v, 0b1000);
}

#[test]
fn int_bor_returns_bitwise_or() {
    let v = run_int("0b1100.bor(0b0011)");
    assert_eq!(v, 0b1111);
}

#[test]
fn int_bxor_returns_bitwise_xor() {
    let v = run_int("0b1100.bxor(0b1010)");
    assert_eq!(v, 0b0110);
}

#[test]
fn int_bnot_flips_every_bit_signed() {
    // i64 NOT of 0 = -1; eval doesn't mask to a narrower width.
    let v = run_int("0.bnot()");
    assert_eq!(v, -1);
}

#[test]
fn int_bsl_shifts_left() {
    let v = run_int("1.bsl(4)");
    assert_eq!(v, 16);
}

#[test]
fn int_bsr_signed_receiver_arithmetic_shift() {
    // Negative i64 right-shifts arithmetically — sign bit
    // propagates, so the result stays negative. Pins the
    // signed-receiver branch of the eval dispatch.
    let v = run_int("(-8).bsr(1)");
    assert_eq!(v, -4);
}

#[test]
fn int_bsr_positive_value_matches_arithmetic_shift() {
    // Sanity: `Int.bsr` on a non-negative receiver behaves like a
    // logical shift since there's no sign bit to propagate.
    let v = run_int("16.bsr(2)");
    assert_eq!(v, 4);
}

// ---------------------------------------------------------------------------
// Narrow-width receivers: the narrow `Bitwise` impls (`UInt8` /
// `Int32` / etc.) only become reachable from a script body via the
// literal-fit coercion at sized param / return slots. Dispatching
// through a typed wrapper exercises both the recorded coercion at
// IR lower time AND the narrow-typed bitwise dispatch at eval time.
// Eval flattens every integer width to `Value::Int(i64)`, so the
// asserted result mirrors the operator's mathematical semantics.
// ---------------------------------------------------------------------------
//
// Driven through the wrapper rather than `0xFF.band(0x0F)` so the
// receiver actually flows through a `UInt8` slot instead of `Int`.

fn run_program_int(source: &str) -> i64 {
    match evaluate_program(&dedent(source)).unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    }
}

#[test]
fn uint8_band_dispatches_through_narrow_impl() {
    let v = run_program_int(
        "
        fn band_u8(x: UInt8, y: UInt8) -> UInt8
          x.band(y)
        end

        fn main
          band_u8(0xFF, 0x0F)
        end
        ",
    );
    assert_eq!(v, 0x0F);
}

#[test]
fn int8_negative_literal_folds_through_narrow_band() {
    // `-1: Int8` flows in via the `-1` literal-fit coercion (typecheck
    // records the negation fold at the call-site span); pinned here
    // through a narrow-typed wrapper.
    let v = run_program_int(
        "
        fn band_i8(x: Int8, y: Int8) -> Int8
          x.band(y)
        end

        fn main
          band_i8(-1, 5)
        end
        ",
    );
    assert_eq!(v, 5);
}

#[test]
fn int32_bsl_dispatches_through_narrow_impl() {
    let v = run_program_int(
        "
        fn shifted(x: Int32) -> Int32
          x.bsl(8)
        end

        fn main
          shifted(1)
        end
        ",
    );
    assert_eq!(v, 256);
}
