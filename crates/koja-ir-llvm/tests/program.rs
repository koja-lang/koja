//! IR-text snapshot tests for the program-mode entry points
//! ([`compile_program`] / [`emit_llvm_ir`]). Pairs with
//! `src/program.rs` in source.
//!
//! Each test drives the full pipeline on a tiny fixture (helpers
//! plus a `fn main` driver, all emitted as plain package functions)
//! and asserts substrings of the produced module text. No linking,
//! no subprocess. Driver e2e tests cover that path.
//!
//! Every emitted module pins the Process-entry trampoline shape via
//! [`assert_program_shape`]: the synthetic test entry's
//! `__entry_wrapper`, an `i32 @main()` trampoline that spawns it,
//! blocks on `koja_rt_main_done`, and returns the
//! `@__koja_exit_code` global's value.
//!
//! Substring (not full-text) assertions because inkwell may adjust
//! attribute ordering between LLVM patch versions.
//!
//! [`compile_program`]: koja_ir_llvm::compile_program
//! [`emit_llvm_ir`]: koja_ir_llvm::emit_llvm_ir

use koja_ast::util::dedent;
use koja_ir_llvm::emit_llvm_ir;

mod common;

use common::{
    APP_NAME, assert_contains, assert_program_shape, extract_function_body,
    lower_program_source as lower,
};

// `<>` concat: String/Binary go inline (`malloc + memcpy`), Bits
// routes through the `__koja_concat_bits` runtime helper.

#[test]
fn binary_concat_helper_emits_inline_malloc_and_memcpy() {
    // Same inline shape as String concat (no trailing NUL though,
    // via `with_nul=false` for `Binary`).
    let source = "
        fn join(a: Binary, b: Binary) -> Binary
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @koja_alloc(i64)");
    assert_contains(&ir_text, "@llvm.memcpy.p0.p0.i64");
    // The runtime concat-bits extern must NOT be declared for a
    // pure-Binary program. Binary stays inline.
    assert!(
        !ir_text.contains("@__koja_concat_bits"),
        "Binary concat should not pull in the bits runtime helper:\n{ir_text}",
    );
}

#[test]
fn binary_literal_emits_malloc_and_byte_packing() {
    // `<<1, 2, 3>>` lowers to BinaryConstruct -> inline malloc(11)
    // (8-byte header + 3 payload bytes), `memset` the payload to
    // zero, then per-byte stores at offsets 0..2. Pin the malloc
    // declaration and the memset zero-init shape since both sit on
    // every BinaryConstruct path.
    let source = "
        fn build -> Binary
          <<1, 2, 3>>
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @koja_alloc(i64)");
    assert_contains(&ir_text, "@llvm.memset.p0.i64");
    // Pure byte-aligned segments must NOT pull in the runtime
    // pack-bits helper. That path is reserved for sub-byte
    // segments.
    assert!(
        !ir_text.contains("@__koja_pack_bits"),
        "byte-aligned BinaryConstruct should not reference the bit-packer:\n{ir_text}",
    );
}

#[test]
fn sub_byte_binary_literal_routes_through_pack_bits() {
    // `<<5::3, 9::4>>` totals 7 bits. Sub-byte alignment means
    // each segment routes through the runtime helper rather than
    // emitting an inline byte-shift loop.
    let source = "
        fn build -> Bits
          <<5::3, 9::4>>
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(
        &ir_text,
        "declare void @__koja_pack_bits(ptr, i64, i8, i64)",
    );
    assert_contains(&ir_text, "call void @__koja_pack_bits(");
}

#[test]
fn bits_concat_helper_routes_through_runtime() {
    // `Bits` has a sub-byte-aligned bit_length and so cannot share
    // the inline `memcpy` shape. `emit_concat`'s `Bits` arm
    // declares and calls the runtime `__koja_concat_bits`
    // helper instead.
    let source = "
        fn join(a: Bits, b: Bits) -> Bits
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @__koja_concat_bits(ptr, ptr)");
    assert_contains(&ir_text, "call ptr @__koja_concat_bits(");
}

// `fn main` body: literals, arithmetic, boolean, comparison
//
// These tests pin that the body compiles cleanly as a plain
// `TestApp.main` helper and that the surrounding Process-entry
// trampoline holds the expected shape.

#[test]
fn fn_main_two_plus_two_emits_plain_helper() {
    let source = "
        fn main -> Int
          2 + 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    let main_body = extract_function_body(&ir_text, "TestApp.main");
    assert!(
        main_body.contains("ret i64"),
        "expected `TestApp.main` to return an i64, got:\n{main_body}",
    );
}

#[test]
fn large_int_literal_compiles_cleanly() {
    let source = "
        fn main -> Int
          5000000000
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn neg_unary_compiles_cleanly() {
    let source = "
        fn main -> Int
          -7
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn logical_and_compiles_cleanly() {
    let source = "
        fn main -> Bool
          true and false
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn logical_or_compiles_cleanly() {
    let source = "
        fn main -> Bool
          true or false
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn not_unary_compiles_cleanly() {
    let source = "
        fn main -> Bool
          not true
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn int_lt_compiles_cleanly() {
    let source = "
        fn main -> Bool
          1 < 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn int_eq_compiles_cleanly() {
    let source = "
        fn main -> Bool
          1 == 1
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
}

#[test]
fn int32_arithmetic_lowers_to_i32_add() {
    // `Int32 + Int32` is now a valid typecheck shape. The
    // operand-width flows through IR `bin_op_result_type` and LLVM
    // `emit_int_binary_op` so the checked add runs at i32, not i64.
    // Pins both the body width and the helper's signature so a
    // future regression in either layer surfaces here.
    let source = "
        fn add32(a: Int32, b: Int32) -> Int32
          a + b
        end

        fn main
          add32(1, 2)
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    let body = extract_function_body(&ir_text, "TestApp.add32");
    assert!(
        body.contains("@llvm.sadd.with.overflow.i32"),
        "expected an i32-width checked add in TestApp.add32 body:\n{body}",
    );
    assert_contains(&ir_text, "define i32 @TestApp.add32(i32 ");
}

// Helper-function definition + call coverage
//
// Pin two things per scenario:
//   1. The helper's `define` line: confirms the IR's
//      [`koja_ir::IRSymbol::mangled`] flows directly through
//      `add_function`.
//   2. The body's `call ...` line: confirms callee lookup and
//      argument plumbing.
//   3. The spawn-driven main shape via `assert_main_shape`.
//
// Param refs from inside a body are still a typecheck feature gap,
// so the helpers below all return constants. The call site is what
// these tests exercise.

#[test]
fn zero_arg_call_emits_helper_define_and_call() {
    let source = "
        fn answer -> Int
          42
        end

        fn main -> Int
          answer()
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.answer()");
    // Helper's body folds to `ret i64 42`.
    assert_contains(&ir_text, "ret i64 42");
    let main_body = extract_function_body(&ir_text, "TestApp.main");
    assert!(
        main_body.contains("call i64 @TestApp.answer()"),
        "expected `TestApp.main` to call `TestApp.answer`:\n{main_body}",
    );
}

#[test]
fn one_arg_call_threads_int_through_helper_signature() {
    // The helper ignores its param (typecheck won't yet lower a
    // body reference to it), but the param shape still has to be
    // emitted on both sides of the call: `define i64 @id(i64 ...)`
    // for the helper and `call i64 @TestApp.id(i64 7)` for main.
    let source = "
        fn id(x: Int) -> Int
          5
        end

        fn main -> Int
          id(7)
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.id(i64");
    let main_body = extract_function_body(&ir_text, "TestApp.main");
    assert!(
        main_body.contains("call i64 @TestApp.id(i64 7)"),
        "expected `TestApp.main` to call `TestApp.id` with `i64 7`:\n{main_body}",
    );
}

#[test]
fn multi_arg_call_threads_each_int_in_declared_order() {
    let source = "
        fn pair(a: Int, b: Int) -> Int
          11
        end

        fn main -> Int
          pair(2, 3)
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_program_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.pair(i64");
    let main_body = extract_function_body(&ir_text, "TestApp.main");
    assert!(
        main_body.contains("call i64 @TestApp.pair(i64 2, i64 3)"),
        "expected `TestApp.main` to call `TestApp.pair`:\n{main_body}",
    );
}
