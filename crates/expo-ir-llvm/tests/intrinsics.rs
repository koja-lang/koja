//! IR-text snapshot tests for `@intrinsic` body emission in
//! `src/intrinsics/`. Pins the LLVM-side dispatch contract:
//!
//! - `Global.print` registers as an emitter that synthesizes a
//!   `define void @"Global.print"(ptr ...) { ... call void
//!   @__expo_print_string(ptr ...); ret void }` shape;
//! - the script body's call site routes through `call void
//!   @"Global.print"(ptr ...)`;
//! - the spawn-driven main trampoline lands `ret i64 0` after the
//!   user body completes, regardless of the trailing expression's
//!   value (scripts always exit 0 on normal completion).
//!
//! Byte-for-byte stdout coverage lives in the lang golden suite;
//! here we pin the static IR shape.

use expo_ast::util::dedent;
use expo_ir::IRScript;
use expo_ir_llvm::emit_script_llvm_ir;

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
    assert_contains(&ir_text, "call void @__expo_print_string(ptr");
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
fn user_main_runs_print_intrinsic_then_returns_void() {
    // The script body is a `Unit`-typed `print(...)` call. With
    // auto-print removed, `__expo_user_main` is the spawn thunk
    // carrying the user body; it should invoke `Global.print` and
    // cap with `ret void`. The trampoline `@main` separately holds
    // `ret i64 0` and never invokes `__expo_print_*` directly
    // — the runtime printer is called only from inside
    // `Global.print`'s synthesized body.
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

    let user_main_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_main_body.contains("call void @Global.print(ptr"),
        "expected `__expo_user_main` to call `Global.print`; got:\n{user_main_body}",
    );
    assert!(
        user_main_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`; got:\n{user_main_body}",
    );
    // The runtime printer must NOT appear directly in
    // `__expo_user_main` — it's reached only via `Global.print`.
    assert!(
        !user_main_body.contains("__expo_print_"),
        "expected `__expo_user_main` not to call the runtime printer directly; got:\n{user_main_body}",
    );

    let trampoline_body = extract_function_body(&ir_text, "main");
    assert!(
        !trampoline_body.contains("__expo_print_"),
        "expected `@main` trampoline not to call the runtime printer directly; got:\n{trampoline_body}",
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
