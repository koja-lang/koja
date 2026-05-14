//! IR-text snapshot tests for the program-mode entry points
//! ([`compile_program`] / [`emit_llvm_ir`]). Pairs with
//! `src/program.rs` in source.
//!
//! Each test drives the full alpha pipeline on a tiny `fn main`
//! fixture and asserts substrings of the produced module text. No
//! linking, no subprocess — driver e2e tests cover that path.
//!
//! Every emitted module pins the spawn-driven main shape: a
//! `define void @__expo_user_main(ptr)` carrying the user body
//! (always returns `ret void`; the trailing expression's value is
//! computed for side effects and discarded), and a `define i64
//! @main()` trampoline that hands `__expo_user_main` to the
//! runtime as PID 1 via `expo_rt_spawn`, blocks on
//! `expo_rt_main_done`, and returns `0`. Scripts and programs
//! always exit `0` on normal completion; user code calls
//! `IO.puts` / `.print()` explicitly for output.
//!
//! Substring (not full-text) assertions because inkwell may adjust
//! attribute ordering between LLVM patch versions.
//!
//! [`compile_program`]: expo_alpha_ir_llvm::compile_program
//! [`emit_llvm_ir`]: expo_alpha_ir_llvm::emit_llvm_ir

use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, extract_function_body,
    lower_program_source as lower,
};

// ---------------------------------------------------------------------------
// `<>` concat: String/Binary go inline (`malloc + memcpy`), Bits
// routes through the `__expo_alpha_concat_bits` runtime helper.
// ---------------------------------------------------------------------------

#[test]
fn binary_concat_helper_emits_inline_malloc_and_memcpy() {
    // Same inline shape as String concat (no trailing NUL though —
    // `with_nul=false` for `Binary`).
    let source = "
        fn join(move a: Binary, move b: Binary) -> Binary
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @malloc(i64)");
    assert_contains(&ir_text, "@llvm.memcpy.p0.p0.i64");
    // The runtime concat-bits extern must NOT be declared for a
    // pure-Binary program — Binary stays inline.
    assert!(
        !ir_text.contains("@__expo_alpha_concat_bits"),
        "Binary concat should not pull in the bits runtime helper:\n{ir_text}",
    );
}

#[test]
fn binary_literal_emits_malloc_and_byte_packing() {
    // `<<1, 2, 3>>` lowers to BinaryConstruct → inline malloc(11)
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

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @malloc(i64)");
    assert_contains(&ir_text, "@llvm.memset.p0.i64");
    // Pure byte-aligned segments must NOT pull in the runtime
    // pack-bits helper — that path is reserved for sub-byte
    // segments.
    assert!(
        !ir_text.contains("@__expo_alpha_pack_bits"),
        "byte-aligned BinaryConstruct should not reference the bit-packer:\n{ir_text}",
    );
}

#[test]
fn sub_byte_binary_literal_routes_through_pack_bits() {
    // `<<5::3, 9::4>>` totals 7 bits — sub-byte alignment means
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

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "declare void @__expo_alpha_pack_bits(ptr, i64, i8, i64)",
    );
    assert_contains(&ir_text, "call void @__expo_alpha_pack_bits(");
}

#[test]
fn bits_concat_helper_routes_through_runtime() {
    // `Bits` has a sub-byte-aligned bit_length and so cannot share
    // the inline `memcpy` shape — `emit_concat`'s `Bits` arm
    // declares and calls the runtime `__expo_alpha_concat_bits`
    // helper instead.
    let source = "
        fn join(move a: Bits, move b: Bits) -> Bits
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare ptr @__expo_alpha_concat_bits(ptr, ptr)");
    assert_contains(&ir_text, "call ptr @__expo_alpha_concat_bits(");
}

// ---------------------------------------------------------------------------
// `fn main` body: literals, arithmetic, boolean, comparison
//
// With auto-print removed, these tests pin that the body compiles
// cleanly into `__expo_user_main` and that the surrounding spawn
// trampoline holds the expected shape. The trailing value is
// discarded (no `__expo_alpha_print_*` calls), so there's no value-
// side substring to anchor on.
// ---------------------------------------------------------------------------

#[test]
fn fn_main_two_plus_two_emits_user_main_ret_void() {
    let source = "
        fn main -> Int
          2 + 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("ret void"),
        "expected `__expo_user_main` to end with `ret void`, got:\n{user_body}",
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
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

    assert_main_shape(&ir_text);
}

#[test]
fn int32_arithmetic_lowers_to_i32_add() {
    // `Int32 + Int32` is now a valid alpha typecheck shape; the
    // operand-width flows through IR `bin_op_result_type` and LLVM
    // `emit_int_binary_op` so the emitted instruction is `add i32`,
    // not `add i64`. Pins both the body width and the helper's
    // signature so a future regression in either layer surfaces here.
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

    assert_main_shape(&ir_text);
    let body = extract_function_body(&ir_text, "TestApp.add32");
    assert!(
        body.contains("add i32"),
        "expected `add i32` in TestApp.add32 body:\n{body}",
    );
    assert_contains(&ir_text, "define i32 @TestApp.add32(i32 ");
}

// ---------------------------------------------------------------------------
// Helper-function definition + call coverage
//
// Pin two things per scenario:
//   1. The helper's `define` line — confirms the IR's
//      [`expo_alpha_ir::IRSymbol::mangled`] flows directly through
//      `add_function`.
//   2. The body's `call ...` line — confirms callee lookup and
//      argument plumbing.
//   3. The spawn-driven main shape via `assert_main_shape`.
//
// Param refs from inside a body are still a typecheck feature gap,
// so the helpers below all return constants. The call site is what
// these tests exercise.
// ---------------------------------------------------------------------------

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

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.answer()");
    // Helper's body folds to `ret i64 42`.
    assert_contains(&ir_text, "ret i64 42");
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("call i64 @TestApp.answer()"),
        "expected `__expo_user_main` to call `TestApp.answer`:\n{user_body}",
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

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.id(i64");
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("call i64 @TestApp.id(i64 7)"),
        "expected `__expo_user_main` to call `TestApp.id` with `i64 7`:\n{user_body}",
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

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @TestApp.pair(i64");
    let user_body = extract_function_body(&ir_text, "__expo_user_main");
    assert!(
        user_body.contains("call i64 @TestApp.pair(i64 2, i64 3)"),
        "expected `__expo_user_main` to call `TestApp.pair`:\n{user_body}",
    );
}
