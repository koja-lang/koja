//! Value-semantics assignment coverage (plus a few binary-literal
//! range checks that share this file). Under value semantics there is
//! no move tracking at all: assignment, argument passing, and `self`
//! receivers are copies, so a binding stays usable after it's read,
//! assigned, or passed. The tests below pin that reads-after-use stay
//! clean.

use koja_ast::util::dedent;

mod common;

use common::{diagnostic_messages, typecheck_file, typecheck_file_fail};

fn assert_clean(source: &str) {
    let checked = typecheck_file(source);
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got: {:?}",
        checked.diagnostics
    );
}

fn assert_diagnostic(source: &str, needle: &str) {
    let failure = typecheck_file_fail(source);
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains(needle)),
        "expected a diagnostic containing `{needle}`; got: {messages:?}"
    );
}

#[test]
fn reading_a_local_after_assigning_it_is_clean() {
    // Value semantics: `p2 = p1` copies, so `p1` stays usable.
    assert_clean(&dedent(
        r#"
        struct Point
          x: Int
          y: Int
        end

        fn main
          p1 = Point{x: 1, y: 2}
          p2 = p1
          p2.y
          p1.x
        end
        "#,
    ));
}

#[test]
fn copy_local_read_after_move_param_call_is_clean() {
    // `Int` is `Copy` — passing it to a `move`-param doesn't
    // strand the original local.
    assert_clean(&dedent(
        r#"
        fn consume(move n: Int) -> Int
          n + 1
        end

        fn main
          x = 42
          consume(x)
          x
        end
        "#,
    ));
}

#[test]
fn reassignment_clears_move_state() {
    assert_clean(&dedent(
        r#"
        fn consume(move s: String) -> Int
          s.length()
        end

        fn main
          greeting = "hello"
          consume(greeting)
          greeting = "world"
          greeting.length()
        end
        "#,
    ));
}

#[test]
fn binary_overflow_bare_segment_diagnoses() {
    assert_diagnostic(
        &dedent(
            r#"
            fn main
              bad = <<256>>
            end
            "#,
        ),
        "does not fit in 8 unsigned bits",
    );
}

#[test]
fn binary_overflow_int8_signed_segment_diagnoses() {
    assert_diagnostic(
        &dedent(
            r#"
            fn main
              bad = <<200 : Int8>>
            end
            "#,
        ),
        "does not fit in 8 signed bits",
    );
}

#[test]
fn binary_in_range_is_clean() {
    assert_clean(&dedent(
        r#"
        fn main
          ok = <<255, 0 : Int8, 65535 : UInt16>>
        end
        "#,
    ));
}
