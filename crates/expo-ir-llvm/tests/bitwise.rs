//! IR-text snapshot tests for the 48-cell `Bitwise` intrinsic family
//! emitted by `src/intrinsics/bitwise.rs`. The auto-imported
//! `Global.bitwise` source brings every `band` / `bor` / `bxor` /
//! `bnot` / `bsl` / `bsr` method into scope, and the LLVM backend's
//! dispatch table routes every cell through one shared emitter that
//! branches on the trailing `.band` / `.bsl` / ... segment.
//!
//! What's pinned here:
//! - one representative cell per op on `Int` (= `Int64`),
//!   demonstrating the LLVM instruction the emitter chose (`and`,
//!   `or`, `xor`, `xor ... -1` for `bnot`, `shl`, `ashr`).
//! - narrow-receiver cells for `UInt8.band` (`and i8`), `UInt8.bsr`
//!   (`lshr i8` — the unsigned-vs-signed shift divergence visible
//!   only at the LLVM layer), and `Int32.bsl` (operand-typed
//!   `shl i32` plus the shift-count `trunc` from the `Int = i64`
//!   second arg). Reachable since literal-fit coercion lets the
//!   user mint `UInt8` / `Int32` values from a literal.

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, extract_function_body, lower_script_source as lower_as_script,
};

/// Drive a script-shaped Expo source through the alpha pipeline,
/// emit textual LLVM IR, and slice out the function body for the
/// intrinsic at `symbol`. Auto-imported `Global.bitwise` definitions
/// only land in the emitted module if their declared shape is
/// reachable from the user's script body, so each test calls the
/// targeted method on an integer literal of the matching width.
fn emit_intrinsic_body(source: &str, symbol: &str) -> String {
    let script = lower_as_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    extract_function_body(&ir_text, symbol).to_string()
}

#[test]
fn int_band_emits_and_i64() {
    // `Int = Int64` at the IR layer, so `Int.band` lowers to
    // `and i64 %0, %1` with no width adjustment.
    let body = emit_intrinsic_body("12.band(10)", "Global.Int.band");
    assert_contains(&body, "and i64 %0, %1");
    assert_contains(&body, "ret i64 %band");
}

#[test]
fn int_bor_emits_or_i64() {
    let body = emit_intrinsic_body("12.bor(3)", "Global.Int.bor");
    assert_contains(&body, "or i64 %0, %1");
    assert_contains(&body, "ret i64 %bor");
}

#[test]
fn int_bxor_emits_xor_i64() {
    let body = emit_intrinsic_body("12.bxor(10)", "Global.Int.bxor");
    assert_contains(&body, "xor i64 %0, %1");
    assert_contains(&body, "ret i64 %bxor");
}

#[test]
fn int_bnot_emits_xor_with_minus_one() {
    // Inkwell's `build_not` lowers to `xor %v, -1` (one-extend of
    // `~0`), which is the canonical LLVM idiom for bitwise NOT on
    // an integer.
    let body = emit_intrinsic_body("0.bnot()", "Global.Int.bnot");
    assert_contains(&body, "xor i64 %0, -1");
    assert_contains(&body, "ret i64 %bnot");
}

#[test]
fn int_bsl_emits_shl_with_native_width_count() {
    // Receiver is `Int = i64`, shift count is `Int = i64`, so the
    // count flows in as-is — no `trunc` needed.
    let body = emit_intrinsic_body("1.bsl(4)", "Global.Int.bsl");
    assert_contains(&body, "shl i64 %0, %1");
    assert!(
        !body.contains("trunc"),
        "expected no shift-count trunc in `Int.bsl`; got:\n{body}",
    );
    assert_contains(&body, "ret i64 %bsl");
}

#[test]
fn int_bsr_signed_receiver_emits_arithmetic_shift() {
    // Signed receiver = arithmetic shift; LLVM idiom is `ashr` (the
    // emitter passes `sign_extend = true` to inkwell's
    // `build_right_shift`, which materializes as `ashr`).
    let body = emit_intrinsic_body("(-8).bsr(1)", "Global.Int.bsr");
    assert_contains(&body, "ashr i64 %0, %1");
    assert_contains(&body, "ret i64 %bsr");
}

// ---------------------------------------------------------------------------
// Narrow-receiver cells. Reachable from a script body via the
// literal-fit coercion at sized param slots. Wrapper-typed call
// args force the literal through the narrow `Bitwise` impl so the
// emitter actually picks the operand-typed instruction.
// ---------------------------------------------------------------------------

#[test]
fn uint8_band_emits_and_i8() {
    // Narrow-receiver `band` lowers to `and i8 %0, %1`; the literal
    // `0xFF` flows in as `Const i8 -1`, exercising the typecheck
    // coercion record at lower time.
    let source = "
        fn band_u8(x: UInt8, y: UInt8) -> UInt8
          x.band(y)
        end

        band_u8(0xFF, 0x0F)
    ";
    let body = emit_intrinsic_body(source, "Global.UInt8.band");
    assert_contains(&body, "and i8 %0, %1");
    assert_contains(&body, "ret i8 %band");
}

#[test]
fn uint8_bsr_emits_logical_shift() {
    // Unsigned-receiver `bsr` lowers to `lshr` (logical right
    // shift), the divergence from signed `Int.bsr` (which lowers
    // to `ashr`). Pinned through a `UInt8`-typed wrapper so the
    // shift actually reaches the unsigned-emitter branch.
    let source = "
        fn shifted(x: UInt8, n: Int) -> UInt8
          x.bsr(n)
        end

        shifted(0xFF, 1)
    ";
    let body = emit_intrinsic_body(source, "Global.UInt8.bsr");
    assert_contains(&body, "lshr i8");
    assert_contains(&body, "ret i8 %bsr");
}

#[test]
fn int32_bsl_emits_shl_with_truncated_shift_count() {
    // `Int32.bsl` takes its receiver as `i32` and its shift count
    // as `Int = i64`; the emitter must `trunc i64 %n to i32`
    // before feeding the count into `shl`. Pinned here so a
    // regression in the count-narrowing path fails this case.
    let source = "
        fn shifted(x: Int32) -> Int32
          x.bsl(8)
        end

        shifted(1)
    ";
    let body = emit_intrinsic_body(source, "Global.Int32.bsl");
    assert_contains(&body, "trunc i64");
    assert_contains(&body, "to i32");
    assert_contains(&body, "shl i32");
    assert_contains(&body, "ret i32 %bsl");
}
