//! IR-text snapshot tests for the program-mode entry points
//! ([`compile_program`] / [`emit_llvm_ir`]). Pairs with
//! `src/program.rs` in source.
//!
//! Each test drives the full alpha pipeline on a tiny `fn main`
//! fixture and asserts substrings of the produced module text. No
//! linking, no subprocess — driver e2e tests cover that path.
//!
//! Every emitted `main` returns `i64 0`; the body's value is fed to
//! a runtime printer first (temporary scaffolding, see
//! [`expo-runtime/src/alpha.rs`](../../../expo-runtime/src/alpha.rs)).
//! The substrings pinned here are: which printer fires, what value
//! lands in the call, and that `main` exits 0.
//!
//! Substring (not full-text) assertions because inkwell may adjust
//! attribute ordering between LLVM patch versions.
//!
//! [`compile_program`]: expo_alpha_ir_llvm::compile_program
//! [`emit_llvm_ir`]: expo_alpha_ir_llvm::emit_llvm_ir

use std::path::PathBuf;

use expo_alpha_ir::{IRProgram, lower_program};
use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const APP_NAME: &str = "emit_test";
const PACKAGE: &str = "TestApp";

fn typecheck(source: &str) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("program.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

fn lower(source: &str) -> IRProgram {
    let checked = typecheck(source);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn assert_contains(ir_text: &str, needle: &str) {
    assert!(
        ir_text.contains(needle),
        "expected `{needle}` in:\n{ir_text}",
    );
}

fn assert_main_shape(ir_text: &str) {
    assert_contains(ir_text, "define i64 @main()");
    assert_contains(ir_text, "ret i64 0");
    // `__expo_app_name` is required by `expo-runtime`'s panic.rs;
    // the alpha backend must emit it on every module so the runtime
    // archive links cleanly regardless of cgu partitioning.
    assert_contains(ir_text, "@__expo_app_name");
}

// ---------------------------------------------------------------------------
// `fn main` body: literals, arithmetic, boolean, comparison
// ---------------------------------------------------------------------------

#[test]
fn fn_main_two_plus_two_prints_four() {
    let source = "
        fn main -> Int
          2 + 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare void @__expo_alpha_print_i64(i64)");
    // inkwell folds `2 + 2` to `i64 4` at const-emission time.
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64(i64 4)");
}

#[test]
fn large_int_literal_prints_i64_constant() {
    let source = "
        fn main -> Int
          5000000000
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "call void @__expo_alpha_print_i64(i64 5000000000)",
    );
}

#[test]
fn neg_unary_prints_negative_int() {
    let source = "
        fn main -> Int
          -7
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64(i64 -7)");
}

#[test]
fn logical_and_prints_false() {
    let source = "
        fn main -> Bool
          true and false
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "declare void @__expo_alpha_print_bool(i64)");
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 0)");
}

#[test]
fn logical_or_prints_true() {
    let source = "
        fn main -> Bool
          true or false
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 1)");
}

#[test]
fn not_unary_prints_false() {
    let source = "
        fn main -> Bool
          not true
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 0)");
}

#[test]
fn int_lt_prints_true() {
    let source = "
        fn main -> Bool
          1 < 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 1)");
}

#[test]
fn int_eq_prints_true() {
    let source = "
        fn main -> Bool
          1 == 1
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call void @__expo_alpha_print_bool(i64 1)");
}

// ---------------------------------------------------------------------------
// Helper-function definition + call coverage
//
// Pin three things per scenario:
//   1. The helper's `define` line — confirms the IR's
//      [`expo_alpha_ir::IRSymbol::mangled`] flows directly through
//      `add_function`.
//   2. The body's `call ...` line — confirms callee lookup and
//      argument plumbing.
//   3. The trailing print-then-exit-0 shape via `assert_main_shape`.
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
    assert_contains(&ir_text, "call i64 @TestApp.answer()");
    // Helper's body folds to `ret i64 42`; main's call result is fed
    // straight to the int printer.
    assert_contains(&ir_text, "ret i64 42");
    assert_contains(&ir_text, "@__expo_alpha_print_i64");
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
    assert_contains(&ir_text, "call i64 @TestApp.id(i64 7)");
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
    assert_contains(&ir_text, "call i64 @TestApp.pair(i64 2, i64 3)");
}
