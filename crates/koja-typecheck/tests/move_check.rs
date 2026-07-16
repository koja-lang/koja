//! Value-semantics assignment coverage (plus a few binary-literal
//! range checks that share this file). Under value semantics there is
//! no ownership tracking at all: assignment, argument passing, and
//! `self` receivers are copies, so a binding stays usable after it's
//! read, assigned, or passed. The tests below pin that reads-after-use
//! stay clean.

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script};

fn assert_clean(source: &str) {
    let checked = typecheck_script(source);
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got: {:?}",
        checked.diagnostics
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

        p1 = Point{x: 1, y: 2}
        p2 = p1
        p2.y
        p1.x
        "#,
    ));
}

#[test]
fn local_read_after_passing_to_call_is_clean() {
    // Value semantics: passing `x` to a call copies, so the
    // original local stays usable afterward.
    assert_clean(&dedent(
        r#"
        fn consume(n: Int) -> Int
          n + 1
        end

        x = 42
        consume(x)
        x
        "#,
    ));
}

#[test]
fn read_after_passing_and_reassigning_is_clean() {
    assert_clean(&dedent(
        r#"
        fn consume(s: String) -> Int
          s.length()
        end

        greeting = "hello"
        consume(greeting)
        greeting = "world"
        greeting.length()
        "#,
    ));
}

#[test]
fn binary_overflow_bare_segment_diagnoses() {
    assert_script_fails_with(
        r#"
        bad = <<256>>
        "#,
        &["does not fit in 8 unsigned bits"],
    );
}

#[test]
fn binary_overflow_int8_signed_segment_diagnoses() {
    assert_script_fails_with(
        r#"
        bad = <<200 : Int8>>
        "#,
        &["does not fit in 8 signed bits"],
    );
}

#[test]
fn binary_in_range_is_clean() {
    assert_clean(&dedent(
        r#"
        ok = <<255, 0 : Int8, 65535 : UInt16>>
        "#,
    ));
}
