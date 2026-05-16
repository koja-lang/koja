//! `@extern "C"` interpreter coverage. The eval backend has no
//! C-runtime linkage, so any FFI call surfaces a clean
//! [`RuntimeError::ExternNotSupported`] — the message points the
//! user at `--backend=llvm`, which does honor extern declarations.
//!
//! Declaring an extern fn but never calling it is fine: lowering
//! produces an [`expo_ir::FunctionKind::Extern`] entry that
//! the interpreter ignores at execution time.

use expo_ast::util::dedent;
use expo_ir_eval::RuntimeError;

mod common;

use common::{PACKAGE, evaluate_program};

#[test]
fn calling_extern_c_fn_surfaces_extern_not_supported() {
    // `Float32` literals don't exist yet, so we exercise the call
    // path through an extern fn that takes zero params and returns
    // an admissible width — `rand32 -> Int32`. The trailing return
    // path drives `rand32()` from inside `main`.
    let source = "
        @extern \"C\"
        fn rand32 -> Int32

        fn driver -> Int32
          rand32()
        end

        fn main -> Int
          driver()
          1
        end
        ";

    let outcome = evaluate_program(&dedent(source));
    let err = outcome.expect_err(
        "calling an extern fn from a regular fn should error at runtime, not return a value",
    );
    assert!(
        matches!(&err, RuntimeError::ExternNotSupported { symbol } if symbol == &format!("{PACKAGE}.rand32")),
        "expected ExternNotSupported(`{PACKAGE}.rand32`), got: {err:?}",
    );
}

#[test]
fn declaring_but_not_calling_extern_c_evaluates_normally() {
    // Declaring `@extern "C" fn cosf(...)` should not cause the
    // interpreter to error — only *invoking* it does. The script
    // returns `7` (Int) without ever touching the extern.
    let source = "
        @extern \"C\"
        @link \"m\"
        fn cosf(x: Float32) -> Float32

        fn main -> Int
          7
        end
        ";

    let value = evaluate_program(&dedent(source))
        .expect("script that never calls the extern should evaluate cleanly");
    assert_eq!(value, expo_ir_eval::Value::Int(7));
}

#[test]
fn extern_error_message_points_at_llvm_backend() {
    let source = "
        @extern \"C\"
        @link \"m:cos\"
        fn host_now -> Int64

        fn driver -> Int64
          host_now()
        end

        fn main -> Int
          driver()
          1
        end
        ";

    let err =
        evaluate_program(&dedent(source)).expect_err("calling an extern from main should error");
    let message = format!("{err}");
    assert!(
        message.contains("--backend=llvm"),
        "extern-not-supported error should point at the llvm backend; got `{message}`",
    );
    assert!(
        message.contains("host_now"),
        "extern-not-supported error should name the offending symbol; got `{message}`",
    );
}
