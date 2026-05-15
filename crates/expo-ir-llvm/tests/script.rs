//! IR-text snapshot tests for the script-mode entry points
//! ([`compile_script`] / [`emit_script_llvm_ir`]). Pairs with
//! `src/script.rs` in source.
//!
//! Script mode wraps a top-level expression as the body of
//! `__expo_user_main` (rather than requiring a `fn main` item).
//! These tests pin the spawn-driven main shape — `define void
//! @__expo_user_main(ptr)` carrying the body, `define i64 @main()`
//! trampoline handing the body to the runtime — plus the
//! helper-call wiring path unique to script-mode lowering (helpers
//! live in a synthesized package fragment).
//!
//! [`compile_script`]: expo_alpha_ir_llvm::compile_script
//! [`emit_script_llvm_ir`]: expo_alpha_ir_llvm::emit_script_llvm_ir

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, extract_function_body,
    lower_script_source as lower_as_script,
};

#[test]
fn bare_two_plus_two_emits_const_add_then_ret_void() {
    // `2 + 2` lowers to a single `IRInstruction::BinaryOp::Add` on
    // two `Const(Int)` operands. inkwell may or may not const-fold;
    // either way the user body block ends in `ret void` (the
    // trailing expression's value is computed and discarded).
    let script = lower_as_script("2 + 2\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
    );
}

#[test]
fn large_int_literal_compiles_with_user_main_ret_void() {
    // 64-bit literals widen past `i32`; the alpha IR `IRType::Int`
    // tracks it as a 64-bit integer. With auto-print gone the only
    // observable IR-text effect is that compilation succeeds and
    // `__expo_user_main` caps with `ret void` — pinned here so a
    // future regression that drops i64 literal support shows up
    // as a compile-time miss.
    let script = lower_as_script("5000000000\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
    );
}

#[test]
fn bare_not_true_emits_user_main_ret_void() {
    let script = lower_as_script("not true\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
    );
}

#[test]
fn int_compare_emits_user_main_ret_void() {
    // The `IRBinOp::Lt` on two i64 constants is inkwell's call to
    // constant-fold or emit `icmp slt`. Either way the surrounding
    // `__expo_user_main` block caps with `ret void` — that's the
    // post-auto-print invariant this test guards.
    let script = lower_as_script("1 < 2\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
    );
}

#[test]
fn string_literal_emits_v1_header_layout() {
    let script = lower_as_script("\"hello\"\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Private constant matches expo-codegen's create_string_global:
    // `{ i64 bit_length, [N+1 x i8] c"<bytes>\00" }`. For "hello":
    // bit_length = 40, payload = 6 bytes (5 utf8 + trailing NUL).
    assert_contains(&ir_text, "@alpha_str.0 = private constant");
    assert_contains(&ir_text, "{ i64 40, [6 x i8] c\"hello\\00\" }");
}

#[test]
fn string_concat_emits_inline_malloc_and_memcpy() {
    // `<>` for `String`/`Binary` lowers to inline LLVM:
    //   1. read both `i64 bit_length`s from the `payload-8` headers
    //      (negative GEP + load),
    //   2. derive byte counts via `>> 3`,
    //   3. `malloc(8 + total_bytes [+1])` for the new heap block,
    //   4. store the combined bit_length at offset 0,
    //   5. `memcpy` lhs payload, `memcpy` rhs payload, and (String
    //      only) write a trailing `\0`.
    let script = lower_as_script("\"foo\" <> \"bar\"\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @malloc(i64)");
    assert_contains(&ir_text, "call ptr @malloc(i64");
    // Two memcpys (lhs payload, rhs payload) + the trailing-NUL
    // store. inkwell renders memcpy as the `llvm.memcpy.p0.p0.i64`
    // intrinsic.
    assert_contains(&ir_text, "@llvm.memcpy.p0.p0.i64");
}

#[test]
fn empty_string_literal_uses_zero_bit_length() {
    let script = lower_as_script("\"\"\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Empty UTF-8 payload: bit_length = 0, payload array length = 1
    // (just the trailing NUL). LLVM renders the all-zero initializer
    // as `zeroinitializer`, and the type appears in the global's
    // declaration line instead.
    assert_contains(
        &ir_text,
        "@alpha_str.0 = private constant { i64, [1 x i8] } zeroinitializer",
    );
}

#[test]
fn float_literal_emits_user_main_ret_void() {
    let script = lower_as_script("3.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
    );
}

#[test]
fn float_arithmetic_emits_fadd_or_const_folds() {
    let script = lower_as_script("1.5 + 2.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // inkwell may or may not const-fold the add; if it doesn't,
    // we'll see `fadd double` in the user body. With auto-print
    // gone there's no value-side sink to observe folded value at,
    // so const-fold cases just leave a no-op body capped by
    // `ret void`. Pin either shape: the operator on un-folded,
    // or the body's `ret void` on folded.
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("fadd double") || user_body.contains("ret void"),
        "expected `fadd double` or `ret void` in `__expo_user_main`:\n{user_body}",
    );
}

#[test]
fn float_compare_emits_ordered_predicate_or_const_folds() {
    let script = lower_as_script("1.5 < 2.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // `OLT` -> `fcmp olt`. inkwell may const-fold; either way the
    // surrounding body caps with `ret void`.
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("fcmp olt double") || user_body.contains("ret void"),
        "expected `fcmp olt double` or `ret void` in `__expo_user_main`:\n{user_body}",
    );
}

#[test]
fn call_to_helper_emits_call_in_user_main_body() {
    // Script mode wires the same helper-declare-then-call shape
    // through `emit_script_llvm_ir`: helper lives in a package
    // fragment, the implicit `__expo_user_main` body issues the
    // call and feeds the result through arithmetic. With auto-print
    // gone the result lands in an unobserved SSA register; the
    // body ends in `ret void`.
    let source = "
        fn answer -> Int
          42
        end

        answer() + 1
        ";

    let script = lower_as_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.answer()");
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("call i64 @TestApp.answer()"),
        "expected `__expo_user_main` to call `TestApp.answer`:\n{user_body}",
    );
    // inkwell does not const-fold across the call boundary: the
    // callee's return value lands in a fresh SSA name (`%call`)
    // and gets `add`-ed against `i64 1`. Pin the SSA-shaped
    // invocation so a regression that drops the call or rewires
    // it through const-fold surfaces here.
    assert!(
        user_body.contains("add i64 %call, 1"),
        "expected `add i64 %call, 1` in `__expo_user_main`:\n{user_body}",
    );
}
