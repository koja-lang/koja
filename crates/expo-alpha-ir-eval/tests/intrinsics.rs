//! Coverage for the eval-side intrinsic dispatch in
//! `src/intrinsics/`. The two fundamental contracts:
//!
//! - `Global.print(s: String)` runs the registered handler, writes
//!   `"{s}\n"` to stdout, and returns [`Value::Unit`].
//! - An unknown mangled symbol routed through
//!   [`crate::intrinsics::dispatch`] surfaces as
//!   [`RuntimeError::UnknownIntrinsic`] — defensive failure mode for
//!   IR that ships an `@intrinsic` the interpreter has no handler
//!   for.
//!
//! Stdout-capturing in-process is fiddly (the runtime printer in
//! the LLVM backend writes via `libc::write`; the eval handler
//! writes via `io::stdout().lock()`). Capturing the stdout stream
//! reliably across threads needs more plumbing than this slice
//! warrants — instead we drive the `Global.print` path through the
//! full pipeline and assert it returns `Value::Unit` without
//! erroring. The byte-for-byte stdout assertion lives in the
//! `expo-driver` e2e suite (`alpha_two_plus_two::*`), where the
//! whole binary's stdout is captured via `Command::output`.

use expo_alpha_ir_eval::{RuntimeError, Value};
use expo_ast::util::dedent;

mod common;

const PACKAGE: &str = "Global";

fn evaluate_script(source: &str) -> Result<Value, RuntimeError> {
    common::evaluate_script_in(PACKAGE, source)
}

#[test]
fn print_intrinsic_returns_unit_when_called_through_script_body() {
    let source = "
        @intrinsic
        fn print(s: String)

        print(\"hello\")
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Unit);
}

#[test]
fn print_intrinsic_via_helper_function_threads_unit_through() {
    let source = "
        @intrinsic
        fn print(s: String)

        fn shout
          print(\"loud\")
        end

        shout()
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Unit);
}

#[test]
fn unknown_intrinsic_surfaces_as_runtime_error() {
    use expo_alpha_ir_eval::Value;

    // The dispatch table itself is private, so we drive the public
    // surface: a script that declares `@intrinsic fn missing` and
    // calls it. `missing` has no registered handler, so the
    // interpreter surfaces `UnknownIntrinsic { symbol: "Global.missing" }`.
    let source = "
        @intrinsic
        fn missing

        missing()
        ";

    let err = evaluate_script(&dedent(source))
        .expect_err("calling an unregistered intrinsic should fail at runtime");
    match err {
        RuntimeError::UnknownIntrinsic { symbol } => {
            assert_eq!(symbol, format!("{PACKAGE}.missing"));
        }
        other => panic!("expected UnknownIntrinsic, got {other:?}"),
    }
    let _ = Value::Unit;
}
