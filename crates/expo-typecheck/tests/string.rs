//! Surface-level coverage for the auto-imported `Global.string`
//! source. Pins that every intrinsic method (`length`, `get`,
//! `byte_length`, `slice`, `to_binary`, `to_cstring`) and the
//! `String` instance of `Equality` / `Hash` register, that the
//! pure-`expo` helpers (`at`, `codepoints`, `empty?`, `join`,
//! `to_int`, `to_float`, ...) register, and that user code can call
//! into both surfaces without the autoimport raising diagnostics.

use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_typecheck::CheckedProgram;

mod common;

use common::typecheck_file as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn string_intrinsic_methods_register() {
    let checked = typecheck("fn main\n  1\nend\n");
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
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["String", "eq"]);
    assert_registered(&checked, &["String", "hash"]);
}

#[test]
fn string_pure_expo_helpers_register() {
    let checked = typecheck("fn main\n  1\nend\n");
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
        fn main -> Int
          \"hello\".length()
        end
        ",
    ));
    typecheck(&dedent(
        "
        fn main -> Int
          \"hello\".byte_length()
        end
        ",
    ));
    typecheck(&dedent(
        "
        fn main -> Bool
          \"hello\".eq(\"hello\")
        end
        ",
    ));
}

#[test]
fn user_code_can_call_pure_expo_helpers() {
    typecheck(&dedent(
        "
        fn main -> Bool
          \"hello\".empty?()
        end
        ",
    ));
    typecheck(&dedent(
        "
        fn main -> String
          \"  hi  \".trim()
        end
        ",
    ));
    typecheck(&dedent(
        "
        fn main -> Bool
          \"hello world\".contains?(\"world\")
        end
        ",
    ));
}
