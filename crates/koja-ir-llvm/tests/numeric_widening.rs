//! IR-text snapshot tests for the `NumericWiden` slice in
//! [`koja_ir_llvm::emit_script_llvm_ir`]: signed sources sign-extend
//! (`sext`), unsigned sources zero-extend (`zext`), and `Float32`
//! extends to `double` (`fpext`). Sign extension is the headline
//! contract — a negative `Int32` (e.g. a C error code) widened into
//! an `Int` slot must stay negative.
//!
//! All assertions are substring-only because LLVM may shuffle
//! attribute ordering between patch versions.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, extract_function_body, lower_script_source};

fn emit(source: &str) -> String {
    let script = lower_script_source(&dedent(source));
    emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed")
}

#[test]
fn int32_widen_emits_sign_extension() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        fn caller(small: Int32) -> Int
          want_int(small)
        end

        caller(7)
        ";
    let ir_text = emit(source);
    let body = extract_function_body(&ir_text, "TestApp.caller");
    assert_contains(body, "sext i32");
    assert_contains(body, "to i64");
}

#[test]
fn uint16_widen_emits_zero_extension() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        fn caller(small: UInt16) -> Int
          want_int(small)
        end

        caller(7)
        ";
    let ir_text = emit(source);
    let body = extract_function_body(&ir_text, "TestApp.caller");
    assert_contains(body, "zext i16");
    assert_contains(body, "to i64");
}

#[test]
fn float32_widen_emits_fpext() {
    let source = "
        fn promote(f: Float32) -> Float
          f
        end

        promote(1.5)
        ";
    let ir_text = emit(source);
    let body = extract_function_body(&ir_text, "TestApp.promote");
    assert_contains(body, "fpext float");
    assert_contains(body, "to double");
}
