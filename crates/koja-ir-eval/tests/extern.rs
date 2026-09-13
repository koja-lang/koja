//! `@extern "C"` interpreter coverage. A symbol with no shim in the
//! dispatch table resolves through the dynamic loader and runs
//! through libffi, so libc and libm calls work under eval the same
//! way they do natively. A symbol the loader cannot find surfaces a
//! clean [`RuntimeError::ExternUnresolved`] at the call, and the
//! message points the user at `--backend=llvm`.
//!
//! Declaring an extern fn but never calling it is fine. Lowering
//! produces an [`koja_ir::FunctionKind::Extern`] entry, resolution
//! records the miss, and nothing reads it until a call.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

mod common;

use common::{PACKAGE, evaluate_script};

#[test]
fn libm_call_resolves_through_the_loader() {
    let source = "
        @extern \"C\"
        @link \"m\"
        fn cos(x: Float64) -> Float64

        cos(0.0)
        ";

    let value = evaluate_script(&dedent(source)).expect("cos resolves through libm");
    assert_eq!(value, Value::Float64(1.0));
}

#[test]
fn libc_call_resolves_through_the_running_process() {
    // No `@link`, so the only place to look is the process itself,
    // which links libc. `abs` also exercises the narrow integer
    // return path, where libffi hands back a register and eval
    // truncates it to the declared `Int32`.
    let source = "
        @extern \"C\"
        fn abs(x: Int32) -> Int32

        abs(-7)
        ";

    let value = evaluate_script(&dedent(source)).expect("abs resolves through the process");
    assert_eq!(value, Value::Int(7));
}

#[test]
fn non_finite_float_return_panics_like_llvm() {
    let source = "
        @extern \"C\"
        @link \"m\"
        fn log(x: Float64) -> Float64

        log(0.0)
        ";

    let err = evaluate_script(&dedent(source)).expect_err("log(0.0) is -inf and must trap");
    match err {
        RuntimeError::Panicked { message } => {
            assert_eq!(message, koja_ir::panics::extern_non_finite_message("log"));
        }
        other => panic!("expected the non-finite panic, got {other:?}"),
    }
}

#[test]
fn calling_unresolvable_extern_surfaces_extern_unresolved() {
    let source = "
        @extern \"C\"
        fn koja_no_such_symbol_anywhere -> Int32

        fn driver -> Int32
          koja_no_such_symbol_anywhere()
        end

        driver()
        1
        ";

    let outcome = evaluate_script(&dedent(source));
    let err = outcome.expect_err(
        "calling an unresolvable extern from a regular fn should error at runtime, not return a value",
    );
    match &err {
        RuntimeError::ExternUnresolved {
            c_name,
            link_lib,
            symbol,
            ..
        } => {
            assert_eq!(c_name, "koja_no_such_symbol_anywhere");
            assert_eq!(link_lib, &None);
            assert_eq!(symbol, &format!("{PACKAGE}.koja_no_such_symbol_anywhere/0"));
        }
        other => panic!("expected ExternUnresolved, got {other:?}"),
    }
}

#[test]
fn declaring_but_not_calling_an_unresolvable_extern_evaluates_normally() {
    // Only *invoking* an unresolvable extern errors. The script
    // returns `7` (Int) without ever touching it.
    let source = "
        @extern \"C\"
        @link \"koja_no_such_library\"
        fn missing(x: Int32) -> Int32

        7
        ";

    let value = evaluate_script(&dedent(source))
        .expect("script that never calls the extern should evaluate cleanly");
    assert_eq!(value, Value::Int(7));
}

#[test]
fn extern_error_message_names_the_library_and_the_llvm_backend() {
    let source = "
        @extern \"C\"
        @link \"koja_no_such_library:host_now\"
        fn host_now -> Int64

        fn driver -> Int64
          host_now()
        end

        driver()
        1
        ";

    let err = evaluate_script(&dedent(source))
        .expect_err("calling an unresolvable extern from the script body should error");
    let message = format!("{err}");
    for expected in [
        "--backend=llvm",
        "host_now",
        "koja_no_such_library",
        "shared library",
    ] {
        assert!(
            message.contains(expected),
            "extern-unresolved error should mention `{expected}`; got `{message}`",
        );
    }
}
