//! End-to-end interpreter coverage for `Debug for String` —
//! specifically that `escape_debug` round-trips correctly through
//! the pipeline. Regression coverage for the cross-arm
//! slot-state leak that surfaced as a SIGABRT inside the
//! `match c` arms when `result = result <> "..."` writes piled up.
//!
//! The bug was a mis-emitted `DropLocal` in `lower_assignment`:
//! lowering arm 2 saw arm 1's post-state (`Owned`) and synthesized
//! a free on the still-Unowned literal at the slot. After the
//! snapshot/restore fix every arm starts from the construct-entry
//! state, so no spurious drop lands. These tests pin the runtime
//! shape against the exact `Debug for String` body shipped in
//! `lib/global/src/debug.expo`.

use expo_ir_eval::Value;

mod common;

use common::evaluate_program as evaluate;

fn run(body: &str) -> Vec<u8> {
    let source = format!("fn main -> String\n  {body}\nend\n");
    match evaluate(&source).expect("evaluation should succeed") {
        Value::String(bytes) => bytes,
        other => panic!("expected `Value::String`, got `{other}`"),
    }
}

#[test]
fn escape_debug_on_plain_ascii_returns_input_unchanged() {
    assert_eq!(run("\"hello\".escape_debug()"), b"hello".to_vec());
}

#[test]
fn escape_debug_on_empty_string_returns_empty_string() {
    assert_eq!(run("\"\".escape_debug()"), b"".to_vec());
}

#[test]
fn escape_debug_escapes_backslash() {
    assert_eq!(run("\"a\\\\b\".escape_debug()"), b"a\\\\b".to_vec());
}

#[test]
fn escape_debug_escapes_double_quote() {
    assert_eq!(run("\"a\\\"b\".escape_debug()"), b"a\\\"b".to_vec());
}

#[test]
fn escape_debug_escapes_newline_carriage_return_and_tab() {
    assert_eq!(run("\"a\\nb\".escape_debug()"), b"a\\nb".to_vec());
    assert_eq!(run("\"a\\rb\".escape_debug()"), b"a\\rb".to_vec());
    assert_eq!(run("\"a\\tb\".escape_debug()"), b"a\\tb".to_vec());
}

#[test]
fn escape_debug_round_trips_through_string_format() {
    // `String.format` is the Debug-protocol body
    // `"\"" <> self.escape_debug() <> "\""`, exercising the
    // concat-after-match path that hit the SIGABRT pre-fix.
    assert_eq!(run("\"hi\".format()"), b"\"hi\"".to_vec());
    assert_eq!(run("\"a\\nb\".format()"), b"\"a\\nb\"".to_vec());
    assert_eq!(run("\"\".format()"), b"\"\"".to_vec());
}
