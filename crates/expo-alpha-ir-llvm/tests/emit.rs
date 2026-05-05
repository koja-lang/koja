//! IR-text snapshot tests for the alpha LLVM backend.
//!
//! Each test drives the full alpha pipeline (`parse_program ->
//! check_program -> lower_{program,script} -> emit_{,script_}llvm_ir`)
//! on a tiny fixture, then asserts the produced module text contains
//! the expected substrings. No linking, no subprocess — those live
//! in `expo-driver`'s end-to-end test, which inherits the runtime /
//! BoringSSL build deps.
//!
//! Coverage splits along the two IR shapes:
//!
//! - `program_*` tests feed an explicit `fn main -> Int / 2 + 2 /
//!   end` source through `lower_program` + `emit_llvm_ir`.
//! - `script_*` tests feed bare `2 + 2\n` through `lower_script` +
//!   `emit_script_llvm_ir` and assert the same `define i64 @main()`
//!   / `ret i64 4` outcome.
//!
//! Substring assertions instead of full-text equality: inkwell may
//! adjust attribute ordering or comment formatting between LLVM
//! patch versions, which would make pinned IR-text fragile. The
//! substrings here check for the lowering rules we actually care
//! about (which instruction shows up, which type, which return).

use std::path::PathBuf;

use expo_alpha_ir::{IRProgram, IRScript, lower_program, lower_script};
use expo_alpha_ir_llvm::{emit_llvm_ir, emit_script_llvm_ir};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("emit.expo"),
            source: source.to_string(),
        }],
        mode,
    );
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

fn lower(source: &str) -> IRProgram {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn lower_as_script(source: &str) -> IRScript {
    let checked = typecheck(source, ParseMode::Script);
    lower_script(&checked).expect("script lowering should succeed")
}

// inkwell's `build_int_add` constant-folds when both operands are
// constants, so `2 + 2` lowers to a literal `i64 4` instead of an
// `add i64 2, 2` instruction. That folding is value-preserving (and
// what we'd want a release build to do regardless) so the assertions
// pin the observable contract — module shape and the final value the
// LLVM main returns — rather than the un-folded instruction text.
#[test]
fn program_fn_main_two_plus_two_emits_i64_main_returning_four() {
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
fn program_large_int_literal_returns_i64_constant() {
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

#[test]
fn script_bare_two_plus_two_emits_i64_main_returning_four() {
    let script = lower_as_script("2 + 2\n");
    let ir_text = emit_script_llvm_ir(&script).expect("emit_script_llvm_ir should succeed");

    assert!(
        ir_text.contains("define i64 @main()"),
        "expected `define i64 @main()` in:\n{ir_text}",
    );
    assert!(
        ir_text.contains("ret i64 4"),
        "expected `ret i64 4` (constant-folded `2 + 2`) in:\n{ir_text}",
    );
}

#[test]
fn script_large_int_literal_returns_i64_constant() {
    let script = lower_as_script("5000000000\n");
    let ir_text = emit_script_llvm_ir(&script).expect("emit_script_llvm_ir should succeed");

    assert!(
        ir_text.contains("ret i64 5000000000"),
        "expected `ret i64 5000000000` in:\n{ir_text}",
    );
}
