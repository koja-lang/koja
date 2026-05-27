//! Coverage for the eval-side intrinsic dispatch in
//! `src/intrinsics/`. `Global.print(s: String)` runs the registered
//! handler, writes `"{s}\n"` to stdout, and returns [`Value::Unit`].
//!
//! Stdout-capturing in-process is fiddly (the runtime printer in
//! the LLVM backend writes via `libc::write`; the eval handler
//! writes via `io::stdout().lock()`). Capturing the stdout stream
//! reliably across threads needs more plumbing than this slice
//! warrants — instead we drive the `Global.print` path through the
//! full pipeline and assert it returns `Value::Unit` without
//! erroring. The byte-for-byte stdout assertion lives in the
//! `koja-driver` e2e suite (`alpha_two_plus_two::*`), where the
//! whole binary's stdout is captured via `Command::output`.
//!
//! Unregistered intrinsic ids (e.g. `@intrinsic fn missing`) used to
//! surface at runtime as [`RuntimeError::UnknownIntrinsic`]; they
//! now fail at lift time because [`koja_ir::IRIntrinsicId`]'s
//! source-axis mapper returns `None` for paths that aren't part of
//! the registered universe. That contract is exercised by
//! `crates/koja-ir/tests/lower_intrinsics.rs`.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

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
