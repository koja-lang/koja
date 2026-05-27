//! Use-after-move enforcement coverage. The typecheck pass owns a
//! per-function `MoveLedger` (per `Resolver::moves`) and flags every
//! read of a local whose move state is `Moved` or `MaybeMoved`. The
//! tests below pin the surface diagnostics for the four trigger
//! sites — assignment RHS, `move`-param call argument, `move self`
//! method receiver, and `move`-param free-fn argument — plus the
//! pessimistic branch-join behavior (`if` / `match`) and the fresh-
//! write reset that lets `x = ...; consume(x); x = ...` keep
//! resolving.

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
fn assigning_one_local_to_another_moves_the_rhs() {
    assert_diagnostic(
        &dedent(
            r#"
            struct Point
              x: Int
              y: Int
            end

            fn main
              p1 = Point{x: 1, y: 2}
              p2 = p1
              p1.x
            end
            "#,
        ),
        "use of moved value `p1`",
    );
}

#[test]
fn move_param_arg_moves_the_caller_local() {
    assert_diagnostic(
        &dedent(
            r#"
            fn consume(move s: String) -> Int
              s.length()
            end

            fn main
              greeting = "hello"
              consume(greeting)
              greeting.length()
            end
            "#,
        ),
        "use of moved value `greeting`",
    );
}

#[test]
fn move_self_method_receiver_moves_the_caller_local() {
    assert_diagnostic(
        &dedent(
            r#"
            struct Counter
              n: Int
            end

            extend Counter
              fn into_value(move self) -> Int
                self.n
              end
            end

            fn main
              c = Counter{n: 1}
              c.into_value()
              c.n
            end
            "#,
        ),
        "use of moved value `c`",
    );
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
fn move_in_if_then_arm_only_yields_maybe_moved() {
    assert_diagnostic(
        &dedent(
            r#"
            fn consume(move s: String) -> Int
              s.length()
            end

            fn run(flag: Bool) -> Int
              greeting = "hello"
              if flag
                consume(greeting)
              end
              greeting.length()
            end
            "#,
        ),
        "may have been moved",
    );
}

#[test]
fn move_in_both_if_arms_yields_strict_moved() {
    assert_diagnostic(
        &dedent(
            r#"
            fn consume(move s: String) -> Int
              s.length()
            end

            fn run(flag: Bool) -> Int
              greeting = "hello"
              if flag
                consume(greeting)
              else
                consume(greeting)
              end
              greeting.length()
            end
            "#,
        ),
        "use of moved value `greeting`",
    );
}

#[test]
fn move_in_only_match_arm_yields_strict_moved() {
    assert_diagnostic(
        &dedent(
            r#"
            fn consume(move s: String) -> Int
              s.length()
            end

            fn run(label: Int) -> Int
              greeting = "hello"
              match label
                0 -> consume(greeting)
                _ -> consume(greeting)
              end
              greeting.length()
            end
            "#,
        ),
        "use of moved value `greeting`",
    );
}

#[test]
fn move_in_one_match_arm_yields_maybe_moved() {
    assert_diagnostic(
        &dedent(
            r#"
            fn consume(move s: String) -> Int
              s.length()
            end

            fn run(label: Int) -> Int
              greeting = "hello"
              match label
                0 -> consume(greeting)
                _ -> 0
              end
              greeting.length()
            end
            "#,
        ),
        "may have been moved",
    );
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
