//! IR-text snapshot tests for `@intrinsic` body emission in
//! `src/intrinsics/`. Pins the LLVM-side dispatch contract:
//!
//! - `Global.print` registers as an emitter that synthesizes a
//!   `define void @"Global.print"(ptr ...) { ... call void
//!   @__expo_alpha_print_string(ptr ...); ret void }` shape;
//! - the script body's call site routes through `call void
//!   @"Global.print"(ptr ...)`;
//! - `Unit`-typed trailings (a script body whose last expression
//!   returns `Unit`, e.g. `print("hello")`) compile cleanly with
//!   `ret i64 0` and no auto-print call from
//!   [`crate::main_wrapper::emit_print_call`].
//!
//! Byte-for-byte stdout coverage lives in the `expo-driver` e2e
//! suite; here we pin the static IR shape.

use expo_alpha_ir::IRScript;
use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::assert_contains;

const APP_NAME: &str = "intrinsics_test";
const PACKAGE: &str = "Global";

fn lower_as_script(source: &str) -> IRScript {
    common::lower_script_source_in(PACKAGE, source)
}

#[test]
fn print_intrinsic_emits_define_void_with_runtime_call() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(\"hi\")
        ";

    let script = lower_as_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "define void @Global.print(ptr");
    assert_contains(&ir_text, "call void @__expo_alpha_print_string(ptr");
    assert_contains(&ir_text, "ret void");
}

#[test]
fn print_intrinsic_call_site_emits_void_call() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(\"hi\")
        ";

    let script = lower_as_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "call void @Global.print(ptr");
}

#[test]
fn unit_typed_trailing_skips_auto_print_in_main() {
    // Script body trailing is a `Unit`-typed `print(...)` call. The
    // emit_as_main path detects `IRType::Unit` and skips the
    // auto-print call entirely; `main` should carry exactly one
    // `ret i64 0` and no `__expo_alpha_print_*` call directly
    // inside `@main` (the printer is invoked from inside
    // `Global.print` instead).
    let source = "
        @intrinsic
        fn print(s: String)

        print(\"silent\")
        ";

    let script = lower_as_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "define i64 @main()");
    assert_contains(&ir_text, "ret i64 0");

    // Pull the `@main` body out and assert the auto-print
    // scaffolding (calls to `__expo_alpha_print_*`) is absent. The
    // `Global.print` body itself contains the `__expo_alpha_print_string`
    // call, which lives outside `@main`.
    let main_body = extract_function_body(&ir_text, "main");
    assert!(
        !main_body.contains("__expo_alpha_print_"),
        "expected `@main` body to skip auto-print on Unit trailing; got:\n{main_body}",
    );
}

/// Slice the LLVM IR text from `define ... @<symbol>(...) {` to its
/// matching closing `}`. Brittle by design — pure structural
/// extraction off the textual IR — but enough for the assertion in
/// the unit-trailing test above.
fn extract_function_body<'a>(ir_text: &'a str, symbol: &str) -> &'a str {
    let needle = format!(" @{symbol}(");
    let define_idx = ir_text
        .find(&needle)
        .or_else(|| {
            let quoted = format!(" @\"{symbol}\"(");
            ir_text.find(&quoted)
        })
        .unwrap_or_else(|| panic!("missing `define ... @{symbol}(` in IR text:\n{ir_text}"));
    let body_start = ir_text[define_idx..]
        .find('{')
        .unwrap_or_else(|| panic!("define for `{symbol}` has no opening `{{`:\n{ir_text}"));
    let body_end = ir_text[define_idx + body_start..]
        .find("\n}")
        .unwrap_or_else(|| panic!("define for `{symbol}` has no closing `}}`:\n{ir_text}"));
    &ir_text[define_idx + body_start..define_idx + body_start + body_end]
}
