//! Shared test scaffolding for the alpha-ir-llvm integration test
//! suite. Each `tests/*.rs` file is its own Cargo test binary, so
//! anything pulled in here lives behind a `mod common;` in the test
//! file. The directory form (`tests/common/mod.rs`) keeps Cargo from
//! picking this up as a test target itself.
//!
//! Every llvm test shape drives `parse → check → lower → emit_*` and
//! asserts substrings of the produced IR text, so we expose:
//!
//! - [`PACKAGE`] / [`APP_NAME`] — defaults every test source registers
//!   under (`"TestApp"` / `"emit_test"`).
//! - [`typecheck`] / [`typecheck_in`] — `parse_program → check_program`
//!   shorthands, parameterized by `ParseMode` (and optionally package
//!   name for tests that target `Global` directly, e.g.
//!   `intrinsics.rs`).
//! - [`lower_program_source`] / [`lower_script_source`] /
//!   [`lower_script_source_in`] — happy-path lowering shorthands.
//! - [`assert_contains`] — substring assertion with a panic message
//!   that includes the full IR text on miss.
//! - [`assert_main_shape`] — pin the wrapper invariants every emitted
//!   module must satisfy: `define i64 @main()`, `ret i64 0`, and the
//!   `@__expo_app_name` global.

// Each `tests/*.rs` file is its own Cargo test binary that only
// pulls a subset of the helpers below, so `dead_code` would fire on
// every helper for every test that doesn't happen to use it. Silence
// it once at the module level rather than peppering individual fns.
#![allow(dead_code)]

use std::path::PathBuf;

use expo_alpha_ir::{IRProgram, IRScript, ProjectEntry, lower_program, lower_script};
use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

pub const PACKAGE: &str = "TestApp";
pub const APP_NAME: &str = "emit_test";

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    typecheck_in(PACKAGE, source, mode)
}

pub fn typecheck_in(package: &str, source: &str, mode: ParseMode) -> CheckedProgram {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
    sources.push(SourceFile {
        package: package.to_string(),
        path: PathBuf::from("test.expo"),
        source: source.to_string(),
    });
    let parsed = parse_program(sources, mode);
    check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"))
}

pub fn lower_program_source(source: &str) -> IRProgram {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, ProjectEntry::Function(entry)).expect("lowering should succeed")
}

pub fn lower_script_source(source: &str) -> IRScript {
    lower_script_source_in(PACKAGE, source)
}

pub fn lower_script_source_in(package: &str, source: &str) -> IRScript {
    let checked = typecheck_in(package, source, ParseMode::Script);
    lower_script(&checked).expect("script lowering should succeed")
}

pub fn assert_contains(ir_text: &str, needle: &str) {
    assert!(
        ir_text.contains(needle),
        "expected `{needle}` in:\n{ir_text}",
    );
}

/// Pin the wrapper invariants every emitted alpha module must satisfy:
///
/// - `define void @__expo_user_main(ptr)` carrying the user body
///   (always returns `ret void`; the trailing expression's value is
///   computed for side effects and discarded);
/// - `define i64 @main()` trampoline that hands the user body to
///   the runtime as PID 1 via `expo_rt_spawn`, blocks on
///   `expo_rt_main_done`, and returns `ret i64 0`;
/// - `@__expo_app_name` global that `expo-runtime`'s panic handler
///   links against.
pub fn assert_main_shape(ir_text: &str) {
    assert_contains(ir_text, "define void @__expo_user_main(ptr");
    assert_contains(ir_text, "define i64 @main()");
    assert_contains(ir_text, "call i64 @expo_rt_spawn(");
    assert_contains(ir_text, "call void @expo_rt_main_done()");
    assert_contains(ir_text, "ret i64 0");
    assert_contains(ir_text, "@__expo_app_name");
}

/// Slice the LLVM textual IR for one function so substring asserts
/// don't accidentally pick up matches from other defs in the same
/// module — relevant for any test where the alpha auto-import pulls
/// stdlib functions (`Global.Int.band`, `DateTime.now`, …) into the
/// emitted IR alongside the user's `main`. Returns the body between
/// the `define ... @<name>(...) {` opening brace and the matching
/// `}` (assumes well-formed LLVM IR with no nested `}` lines, which
/// holds for everything we emit today).
pub fn extract_function_body<'a>(ir_text: &'a str, name: &str) -> &'a str {
    let header = format!("@{name}(");
    let header_idx = ir_text
        .find(&header)
        .unwrap_or_else(|| panic!("function `@{name}` not found in IR:\n{ir_text}"));
    let open = ir_text[header_idx..]
        .find('{')
        .unwrap_or_else(|| panic!("opening brace of `@{name}` missing in IR:\n{ir_text}"))
        + header_idx;
    let close = ir_text[open..]
        .find("\n}")
        .unwrap_or_else(|| panic!("closing brace of `@{name}` missing in IR:\n{ir_text}"))
        + open;
    &ir_text[open..close]
}
