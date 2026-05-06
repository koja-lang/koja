//! IR-text snapshot tests for the script-mode entry points
//! ([`compile_script`] / [`emit_script_llvm_ir`]). Pairs with
//! `src/script.rs` in source.
//!
//! Script mode wraps a top-level expression as the body of `main`
//! (rather than requiring a `fn main` item). These tests pin the
//! same auto-print + `ret i64 0` shape as program-mode plus the
//! helper-call wiring path that's unique to script-mode lowering
//! (helpers live in a synthesized package fragment).
//!
//! [`compile_script`]: expo_alpha_ir_llvm::compile_script
//! [`emit_script_llvm_ir`]: expo_alpha_ir_llvm::emit_script_llvm_ir

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower_as_script,
};

#[test]
fn bare_two_plus_two_prints_four() {
    let script = lower_as_script("2 + 2\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64(i64 4)");
}

#[test]
fn large_int_literal_prints_i64_constant() {
    let script = lower_as_script("5000000000\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "call void @__expo_alpha_print_i64(i64 5000000000)",
    );
}

#[test]
fn bare_not_true_prints_false() {
    let script = lower_as_script("not true\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare void @__expo_alpha_print_bool(i64)");
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 0)");
}

#[test]
fn int_compare_prints_true() {
    let script = lower_as_script("1 < 2\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 1)");
}

#[test]
fn string_literal_emits_v1_header_layout_and_print_string_call() {
    let script = lower_as_script("\"hello\"\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Private constant matches expo-codegen's create_string_global:
    // `{ i64 bit_length, [N+1 x i8] c"<bytes>\00" }`. For "hello":
    // bit_length = 40, payload = 6 bytes (5 utf8 + trailing NUL).
    assert_contains(&ir_text, "@alpha_str.0 = private constant");
    assert_contains(&ir_text, "{ i64 40, [6 x i8] c\"hello\\00\" }");
    assert_contains(&ir_text, "declare void @__expo_alpha_print_string(ptr)");
    assert_contains(&ir_text, "call void @__expo_alpha_print_string(ptr ");
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
fn float_literal_emits_double_const_and_print_f64_call() {
    let script = lower_as_script("3.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare void @__expo_alpha_print_f64(double)");
    assert_contains(&ir_text, "call void @__expo_alpha_print_f64(double 3.5");
}

#[test]
fn float_arithmetic_emits_fadd() {
    let script = lower_as_script("1.5 + 2.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // inkwell may or may not const-fold the add; pin the operator
    // and the f64 print sink so either lowering passes.
    assert!(
        ir_text.contains("fadd double") || ir_text.contains("@__expo_alpha_print_f64(double 4."),
        "expected `fadd double` or folded f64 print of 4.0:\n{ir_text}",
    );
}

#[test]
fn float_compare_emits_ordered_predicate() {
    let script = lower_as_script("1.5 < 2.5\n");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // `OLT` -> `fcmp olt`. inkwell may const-fold, in which case
    // the comparison vanishes and we land on the print(true=1) sink.
    assert!(
        ir_text.contains("fcmp olt double")
            || ir_text.contains("call void @__expo_alpha_print_bool(i64 1)"),
        "expected `fcmp olt double` or folded bool=true print:\n{ir_text}",
    );
}

#[test]
fn call_to_helper_emits_call_in_main_body() {
    // Script mode wires the same helper-declare-then-call shape
    // through `emit_script_llvm_ir`: helper lives in a package
    // fragment, the implicit `main` body issues the call and feeds
    // the result through arithmetic before printing.
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
    assert_contains(&ir_text, "call i64 @TestApp.answer()");
    // inkwell does not const-fold across the call boundary: the
    // callee's return value lands in a fresh SSA name (`%call`),
    // gets `add`-ed against `i64 1`, and the resulting `%add` is
    // what the printer receives. Pin the SSA-shaped invocation
    // rather than the value `43`.
    assert_contains(&ir_text, "%add = add i64 %call, 1");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64(i64 %add)");
}
