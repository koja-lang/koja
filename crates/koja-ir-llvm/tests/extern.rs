//! IR-text snapshot tests for `@extern "C"` declaration emission.
//!
//! Pins the LLVM-side contract:
//!
//! - bare `@extern "C" fn cosf(x: Float32) -> Float32` declares
//!   `declare float @cosf(float)` (no body, the C runtime
//!   provides the implementation at link time).
//! - aliased `@extern "C" @link "m:cos"` declares the function
//!   under the `link_name`, not the mangled symbol. The user
//!   `IRSymbol::TestApp.cosf` resolves to LLVM `@cos`.
//! - call sites resolve through the `IRSymbol -> FunctionValue`
//!   index on [`koja_ir_llvm::ctx::EmitContext`], so
//!   `relay(x: Float32) -> Float32 = cosf(x)` emits
//!   `call float @cos(...)` against the aliased name even though
//!   the IR call carries the internal symbol.
//! - `CPtr<T>` lowers to an opaque LLVM `ptr` in both parameter
//!   and return position with no marshaling shim, so an
//!   `@extern "C" fn write(buf: CPtr<UInt8>, len: UInt64) ->
//!   Int32` declaration matches the C ABI directly and call
//!   sites pass `CPtr` SSA values straight through.

use koja_ast::util::dedent;
use koja_ir::IRScript;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, PACKAGE, assert_contains, lower_script_source_in};

fn lower(source: &str) -> IRScript {
    lower_script_source_in(PACKAGE, source)
}

#[test]
fn bare_extern_c_emits_declare_under_bare_last_segment() {
    let source = "
        @extern \"C\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare float @cosf(float)");
    assert!(
        !ir_text.contains("define float @cosf"),
        "extern fn must not emit a body; got:\n{ir_text}",
    );
    assert!(
        !ir_text.contains(&format!("@{PACKAGE}.cosf")),
        "extern fn declares under the bare last-segment name, not the mangled \
         symbol; got:\n{ir_text}",
    );
}

#[test]
fn link_only_lib_emits_declare_under_bare_last_segment() {
    let source = "
        @extern \"C\"
        @link \"m\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    // `@link "m"` only sets `link_lib`. The C symbol is still the
    // function's bare last segment.
    assert_contains(&ir_text, "declare float @cosf(float)");
}

#[test]
fn aliased_link_emits_declare_under_link_name() {
    let source = "
        @extern \"C\"
        @link \"m:cos\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare float @cos(float)");
    assert!(
        !ir_text.contains("declare float @cosf"),
        "aliased extern must not declare under its mangled name; got:\n{ir_text}",
    );
}

#[test]
fn call_site_resolves_through_aliased_link_name() {
    let source = "
        @extern \"C\"
        @link \"m:cos\"
        fn cosf(x: Float32) -> Float32

        fn relay(x: Float32) -> Float32
          cosf(x)
        end
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare float @cos(float)");
    assert_contains(&ir_text, "call float @cos(float");
    // The internal name must not leak into the emitted call.
    assert!(
        !ir_text.contains("@cosf("),
        "call site should resolve through link_name `cos`, not mangled name `cosf`; got:\n{ir_text}",
    );
}

#[test]
fn cptr_param_lowers_to_opaque_pointer() {
    let source = "
        @extern \"C\"
        @link \"c\"
        fn malloc(size: UInt64) -> CPtr<UInt8>
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare ptr @malloc(i64)");
}

#[test]
fn extern_with_cptr_arg_declares_pointer_param() {
    // `CPtr<T>` in argument position lowers to an opaque LLVM
    // `ptr`, the same shape as the return-position case above and
    // ABI-compatible with C `void*` for any T.
    let source = "
        @extern \"C\"
        @link \"ffi_helper\"
        fn write_byte(buf: CPtr<UInt8>, value: UInt8) -> Int32
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare i32 @write_byte(ptr, i8)");
}

#[test]
fn call_to_extern_passes_cptr_as_ptr_with_no_marshaling() {
    // The call site for an extern fn taking a `CPtr<T>` argument
    // must hand the SSA `ptr` straight through. No per-call
    // marshaling shim, no allocas around the ptr value.
    let source = "
        @extern \"C\"
        @link \"c\"
        fn malloc(size: UInt64) -> CPtr<UInt8>

        @extern \"C\"
        @link \"c\"
        fn free(p: CPtr<UInt8>)

        fn alloc_and_drop(size: UInt64) -> Unit
          buf = malloc(size)
          free(buf)
        end
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "declare ptr @malloc(i64)");
    assert_contains(&ir_text, "declare void @free(ptr)");
    assert_contains(&ir_text, "call ptr @malloc(i64");
    assert_contains(&ir_text, "call void @free(ptr");
}
