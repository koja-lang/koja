//! Surface-level coverage for the auto-imported `Global.string`
//! source. Pins that every intrinsic method (`length`, `get`,
//! `byte_length`, `slice`, `to_binary`, `to_cstring`) and the
//! `String` instance of `Equality` / `Hash` register, that the
//! pure-`koja` helpers (`at`, `codepoints`, `empty?`, `join`,
//! `to_int`, `to_float`, ...) register, and that user code can call
//! into both surfaces without the autoimport raising diagnostics.

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::typecheck_script as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn string_intrinsic_methods_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["String", "ConversionError"]);
    for method in [
        "byte_length",
        "get",
        "length",
        "slice",
        "to_binary",
        "to_cstring",
    ] {
        assert_registered(&checked, &["String", method]);
    }
}

#[test]
fn string_equality_and_hash_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["String", "eq"]);
    assert_registered(&checked, &["String", "hash"]);
}

#[test]
fn string_pure_koja_helpers_register() {
    let checked = typecheck("1\n");
    for method in [
        "alpha?",
        "at",
        "codepoints",
        "contains?",
        "digit?",
        "downcase",
        "empty?",
        "ends_with?",
        "escape_debug",
        "graphemes",
        "join",
        "replace",
        "reverse",
        "split",
        "starts_with?",
        "to_float",
        "to_int",
        "trim",
        "trim_end",
        "trim_start",
        "upcase",
        "whitespace?",
    ] {
        assert_registered(&checked, &["String", method]);
    }
}

#[test]
fn user_code_can_call_string_intrinsics() {
    typecheck(&dedent(
        "
        \"hello\".length()
        ",
    ));
    typecheck(&dedent(
        "
        \"hello\".byte_length()
        ",
    ));
    typecheck(&dedent(
        "
        \"hello\".eq(\"hello\")
        ",
    ));
}

#[test]
fn user_code_can_call_pure_koja_helpers() {
    typecheck(&dedent(
        "
        \"hello\".empty?()
        ",
    ));
    typecheck(&dedent(
        "
        \"  hi  \".trim()
        ",
    ));
    typecheck(&dedent(
        "
        \"hello world\".contains?(\"world\")
        ",
    ));
}
