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

use std::path::PathBuf;

use expo_alpha_ir::{IRScript, lower_script};
use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const APP_NAME: &str = "emit_test";
const PACKAGE: &str = "TestApp";

fn typecheck(source: &str) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("script.expo"),
            source: source.to_string(),
        }],
        ParseMode::Script,
    );
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

fn lower_as_script(source: &str) -> IRScript {
    let checked = typecheck(source);
    lower_script(&checked).expect("script lowering should succeed")
}

fn assert_contains(ir_text: &str, needle: &str) {
    assert!(
        ir_text.contains(needle),
        "expected `{needle}` in:\n{ir_text}",
    );
}

fn assert_main_shape(ir_text: &str) {
    assert_contains(ir_text, "define i64 @main()");
    assert_contains(ir_text, "ret i64 0");
    assert_contains(ir_text, "@__expo_app_name");
}

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
