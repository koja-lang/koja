//! IR-text snapshot tests for `@intrinsic` body emission in
//! `src/intrinsics/`, driven against the real stdlib declarations
//! (no fixture decls). Pins the LLVM-side dispatch contract:
//!
//! - the raw socket intrinsics from the real `Net` package emit
//!   bodies that call the `koja_socket_*` runtime helpers without
//!   materializing domain address structs
//! - the spawn-driven main trampoline lands `ret i64 0` after the
//!   user body completes, regardless of the trailing expression's
//!   value (scripts always exit 0 on normal completion)
//!
//! Byte-for-byte stdout coverage lives in the lang golden suite.
//! Here we pin the static IR shape.

use koja_ast::util::dedent;
use koja_ir::IRScript;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{assert_contains, extract_function_body};

const APP_NAME: &str = "intrinsics_test";

fn lower_as_script(source: &str) -> IRScript {
    common::lower_script_source(&dedent(source))
}

#[test]
fn socket_intrinsics_emit_only_raw_result_shapes() {
    // Any script body works. The qualified bundle lowers the whole
    // `Net` package, so the raw socket intrinsics emit regardless
    // of reachability.
    let script = common::lower_script_source_qualified("1");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let recv_from_body = extract_function_body(&ir_text, "Net.Socket.recv_from_raw/2");
    let resolve_body = extract_function_body(&ir_text, "Net.Socket.resolve_raw/1");
    assert_contains(recv_from_body, "call ptr @koja_socket_recv_from(");
    assert_contains(resolve_body, "call ptr @koja_socket_resolve(");
    for body in [recv_from_body, resolve_body] {
        assert!(
            !body.contains("IPAddress") && !body.contains("Socket.Address"),
            "socket intrinsic IR must not materialize domain address structs:\n{body}",
        );
    }
}

#[test]
fn string_next_emits_utf8_runtime_call_and_option_branches() {
    let source = "
        \"é\".next(0)
        ";

    let script = lower_as_script(source);
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "call ptr @koja_string_next(");
    assert_contains(&ir_text, "with_character");
    assert_contains(&ir_text, "with_next");
}

#[test]
fn map_next_emits_bucket_cursor_scan() {
    let source = "
        map: Map<String, Int> = [\"key\": 1]
        map.next(0)
        ";

    let script = lower_as_script(source);
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "cursor.state");
    assert_contains(&ir_text, "cursor.occupied");
    assert_contains(&ir_text, "cursor.result_next");
}

#[test]
fn set_next_emits_bucket_cursor_scan() {
    let source = "
        set: Set<String> = [\"item\"]
        set.next(0)
        ";

    let script = lower_as_script(source);
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "cursor.state");
    assert_contains(&ir_text, "cursor.occupied");
    assert_contains(&ir_text, "cursor.result_next");
}

#[test]
fn user_main_runs_stdout_call_then_returns_void() {
    // The script body is a `Unit`-typed `IO.puts(...)` call.
    // `__koja_user_main` is the spawn thunk carrying the user body.
    // It should invoke `Global.IO.puts` and cap with `ret void`. The
    // trampoline `@main` separately holds `ret i64 0` and never
    // writes to stdout itself.
    let script = lower_as_script("IO.puts(\"silent\")");
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "define i64 @main()");
    assert_contains(&ir_text, "ret i64 0");

    let user_main_body = extract_function_body(&ir_text, "__koja_user_main");
    assert!(
        user_main_body.contains("call void @\"Global.IO.puts/1\"(ptr"),
        "expected `__koja_user_main` to call `Global.IO.puts`; got:\n{user_main_body}",
    );
    assert!(
        user_main_body.contains("ret void"),
        "expected `__koja_user_main` to end with `ret void`; got:\n{user_main_body}",
    );

    let trampoline_body = extract_function_body(&ir_text, "main");
    assert!(
        !trampoline_body.contains("Global.IO.puts"),
        "expected `@main` trampoline not to write to stdout directly; got:\n{trampoline_body}",
    );
}
