//! IR-text snapshot coverage for the `Clone` protocol's heap-
//! primitive intrinsics. Pins the static LLVM emission shape:
//!
//! - each of the three `@intrinsic` impls lands as a `define ptr
//!   @"Global.<Type>.clone"(ptr ...)` whose body allocates fresh
//!   storage via `@malloc`, copies the header + payload via the
//!   `@memcpy` extern, and returns the new payload pointer;
//! - `String.clone` writes a trailing NUL (a separate `store i8 0`
//!   past the payload), `Binary.clone` and `Bits.clone` do not;
//! - the user call site emits `call ptr @"Global.<Type>.clone"(...)`.
//!
//! Byte-for-byte runtime coverage (clone produces an independent
//! buffer, mutating one doesn't disturb the other) lives in the
//! eval suite (`koja-ir-eval/tests/clone.rs`) and the driver
//! e2e (`crates/koja-driver/tests/alpha_two_plus_two.rs`).

use koja_ast::util::dedent;
use koja_ir_llvm::emit_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, extract_function_body, lower_program_source as lower};

#[test]
fn string_clone_emits_malloc_memcpy_and_trailing_nul() {
    let source = "
        fn copy(s: String) -> String
          s.clone()
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "declare ptr @malloc(i64)");
    assert_contains(&ir_text, "declare ptr @memcpy(ptr, ptr, i64)");
    assert_contains(&ir_text, "define ptr @Global.String.clone(ptr");

    let body = extract_function_body(&ir_text, "Global.String.clone");
    assert!(
        body.contains("call ptr @malloc("),
        "String.clone body must call malloc; got:\n{body}",
    );
    assert!(
        body.contains("call ptr @memcpy("),
        "String.clone body must call memcpy; got:\n{body}",
    );
    assert!(
        body.contains("store i8 0,"),
        "String.clone body must store a trailing NUL byte; got:\n{body}",
    );
}

#[test]
fn binary_clone_emits_malloc_memcpy_and_no_nul() {
    let source = "
        fn copy(b: Binary) -> Binary
          b.clone()
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "define ptr @Global.Binary.clone(ptr");

    let body = extract_function_body(&ir_text, "Global.Binary.clone");
    assert!(
        body.contains("call ptr @malloc("),
        "Binary.clone body must call malloc; got:\n{body}",
    );
    assert!(
        body.contains("call ptr @memcpy("),
        "Binary.clone body must call memcpy; got:\n{body}",
    );
    assert!(
        !body.contains("store i8 0,"),
        "Binary.clone body must not write a trailing NUL byte; got:\n{body}",
    );
}

#[test]
fn bits_clone_emits_ceil_byte_count_and_no_nul() {
    let source = "
        fn copy(b: Bits) -> Bits
          b.clone()
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "define ptr @Global.Bits.clone(ptr");

    let body = extract_function_body(&ir_text, "Global.Bits.clone");
    assert!(
        body.contains("call ptr @malloc("),
        "Bits.clone body must call malloc; got:\n{body}",
    );
    assert!(
        body.contains("call ptr @memcpy("),
        "Bits.clone body must call memcpy; got:\n{body}",
    );
    // Bits ceils the byte count via `(bits + 7) >> 3`, so the body
    // must contain a `+ 7` constant ahead of the `lshr` (or `ashr`)
    // by 3. We pin the source-side `add ... 7` token to catch a
    // future regression that drops the ceiling.
    assert!(
        body.contains(", 7"),
        "Bits.clone body must add 7 before shifting (ceil-byte rounding); got:\n{body}",
    );
    assert!(
        !body.contains("store i8 0,"),
        "Bits.clone body must not write a trailing NUL byte; got:\n{body}",
    );
}

#[test]
fn user_call_to_string_clone_emits_named_call() {
    let source = "
        fn main -> String
          \"hi\".clone()
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "call ptr @Global.String.clone(ptr");
}
