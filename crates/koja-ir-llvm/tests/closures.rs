//! IR-text snapshot tests for closure emission.
//!
//! Pinned shapes:
//!
//! - **Closure ABI**: `FunctionKind::Closure` bodies declare an
//!   extra `ptr` parameter at LLVM position 0 (the env pointer).
//! - **`MakeClosure`**: malloc the env block (or null for the
//!   captureless adapter shape), store each capture, and pack the
//!   `{fn_ptr, env_ptr}` fat pointer for downstream use.
//! - **`CallClosure`**: extract the fat-pointer fields and dispatch
//!   indirectly, prepending the env pointer to user args.
//! - **`LoadCapture`**: `getelementptr inbounds` into the body's env
//!   block followed by a typed `load`.
//! - **Fn-as-value adapter** dispatches through the same fat-pointer
//!   shape with a null env.
//!
//! Driven through script mode: the closures live in the top-level
//! script body, which the backend lowers into the `__koja_user_main`
//! wrapper. Closures hoisted out of the script body mangle as
//! `__script_body__closure<N>` (rather than `main__closure<N>`).

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower};

#[test]
fn closure_body_declares_env_pointer_param() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        f(5)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    // The closure body's signature: env_ptr (ptr) first, then the
    // user-visible `x: Int` (i64).
    assert_contains(&ir_text, "define i64 @TestApp.__script_body__closure0(ptr ");
}

#[test]
fn make_closure_with_capture_mallocs_and_stores_into_env() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        f(5)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_contains(&ir_text, "__script_body__closure0.env");
    // The env block is allocated by libc malloc.
    assert_contains(&ir_text, "call ptr @koja_alloc");
    // The capture-bearing fat pointer ends up at a load with the
    // closure-shaped struct type `{ ptr, ptr }`.
    assert_contains(&ir_text, "load { ptr, ptr }");
}

#[test]
fn call_closure_dispatches_indirectly_with_env_first() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        f(5)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    // CallClosure spills the fat-pointer to alloca, GEPs the two
    // halves, then dispatches via an indirect call. Inkwell prints
    // indirect calls without a `@symbol`; matching the GEP labels
    // is enough to anchor the shape without coupling to inkwell's
    // exact rendering of the call site.
    assert_contains(&ir_text, "closure_call.fn_ptr");
    assert_contains(&ir_text, "closure_call.env_ptr");
}

#[test]
fn load_capture_indexes_through_env_struct() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        f(5)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    // `LoadCapture` GEPs inside the body's env struct. The label
    // is `env.<index>`; the load names its dest `capture.<index>`.
    assert_contains(&ir_text, "env.0");
    assert_contains(&ir_text, "capture.0");
}

#[test]
fn fn_as_value_wrapper_emits_make_closure_with_null_env() {
    let source = "
        fn add(x: Int, y: Int) -> Int
          x + y
        end

        fn apply(f: fn (Int, Int) -> Int, x: Int, y: Int) -> Int
          f(x, y)
        end

        apply(add, 1, 2)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    // The wrapper body for `add` carries the closure ABI (env-first),
    // and `MakeClosure` for the captureless shape stores null into
    // the env slot.
    assert_contains(&ir_text, "define i64 @TestApp.add__as_closure(ptr ");
    // `apply` is a regular function whose `f` parameter is the fat
    // pointer struct.
    assert_contains(&ir_text, "@TestApp.apply({ ptr, ptr }");
    // The captureless wrapper stores `null` as the env slot.
    assert_contains(&ir_text, "store ptr null,");
}

#[test]
fn heap_capture_closure_emits_drop_env_glue_and_rc_dec_teardown() {
    let source = "
        greeting = \"hi\"
        f = fn (x: Int) -> Int
          x + greeting.length()
        end
        f(5)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    // The env block carries the `[i64 rc][ptr drop_fn]` header: rc is
    // stamped to 1 and the capture-release glue address is stored.
    assert_contains(&ir_text, "__script_body__closure0.env.rc");
    assert_contains(&ir_text, "__script_body__closure0.env.drop_fn");
    // A `$drop_env$` capture-release glue is synthesized for the
    // String-capturing closure (closure-shaped, env-first ABI).
    assert_contains(&ir_text, "$drop_env$");
    // The closure slot is torn down through the runtime rc-dec, which
    // runs the glue + frees the env at zero.
    assert_contains(&ir_text, "@koja_closure_rc_dec");
}

#[test]
fn closure_body_loads_user_param_from_alloca_after_env() {
    let source = "
        f = fn (x: Int) -> Int
          x + 1
        end
        f(41)
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    // The captureless body still exposes the env-first ABI; user
    // params follow it normally.
    assert_contains(&ir_text, "define i64 @TestApp.__script_body__closure0(ptr ");
}
