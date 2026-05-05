//! IR-text snapshot tests for the alpha LLVM backend.
//!
//! Each test drives the full alpha pipeline (`parse_program ->
//! check_program -> lower_program -> emit_llvm_ir`) on a tiny
//! fixture, then asserts the produced module text contains the
//! expected substrings. No linking, no subprocess — those live in
//! `expo-driver`'s end-to-end test, which inherits the runtime /
//! BoringSSL build deps.
//!
//! Substring assertions instead of full-text equality: inkwell may
//! adjust attribute ordering or comment formatting between LLVM
//! patch versions, which would make pinned IR-text fragile. The
//! substrings here check for the lowering rules we actually care
//! about (which instruction shows up, which type, which return).

use std::path::PathBuf;

use expo_alpha_ir::{IRProgram, lower_program};
use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("emit.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

// inkwell's `build_int_add` constant-folds when both operands are
// constants, so `2 + 2` lowers to a literal `i64 4` instead of an
// `add i64 2, 2` instruction. That folding is value-preserving (and
// what we'd want a release build to do regardless) so the assertions
// pin the observable contract — module shape and the final value the
// LLVM main returns — rather than the un-folded instruction text.
#[test]
fn fn_main_two_plus_two_emits_i64_main_returning_four() {
    let source = "
        fn main -> Int
          2 + 2
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program).expect("emit_llvm_ir should succeed");

    assert!(
        ir_text.contains("define i64 @main()"),
        "expected `define i64 @main()` in:\n{ir_text}",
    );
    assert!(
        ir_text.contains("ret i64 4"),
        "expected `ret i64 4` (constant-folded `2 + 2`) in:\n{ir_text}",
    );
}

// Use a non-folded shape: subtraction is not yet supported, but a
// `ret i64 <large>` exercises the const-emission path for a value
// beyond i32 range, sanity-checking that we route through `i64`
// constants and not a narrower width.
#[test]
fn large_int_literal_returns_i64_constant() {
    let source = "
        fn main -> Int
          5000000000
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program).expect("emit_llvm_ir should succeed");

    assert!(
        ir_text.contains("ret i64 5000000000"),
        "expected `ret i64 5000000000` in:\n{ir_text}",
    );
}
