//! Return-mode inference over lowered user functions. Exercises the
//! `owned()` rule end-to-end: catalog lookups reached through a call,
//! aggregate-payload borrowing, `move`-through, statics, and the
//! recursion / tail-call cycle bias toward `Borrowed`.

mod common;

use common::{function, lower_program_source};
use koja_ast::ast::ReturnMode;

/// One program holding every shape under test, lowered once.
fn program() -> koja_ir::IRProgram {
    lower_program_source(
        "
        struct Wrapper
          value: String
        end

        enum Opt
          None
          Some(String)
        end

        fn cat(a: String, b: String) -> String
          a <> b
        end

        fn cloned(s: String) -> String
          s.clone()
        end

        fn as_bin(s: String) -> Binary
          s.to_binary()
        end

        fn literal_str() -> String
          \"hello\"
        end

        fn passed_through(move s: String) -> String
          s
        end

        fn wrap_field(w: Wrapper) -> Opt
          Opt.Some(w.value)
        end

        fn countdown(n: Int) -> String
          if n <= 0
            \"done\"
          else
            countdown(n - 1)
          end
        end

        fn main
          result = cat(\"a\", \"b\")
        end
        ",
    )
}

#[test]
fn concat_result_is_owned() {
    assert_eq!(function(&program(), "cat").return_mode, ReturnMode::Owned);
}

#[test]
fn owning_intrinsic_call_threads_through_as_owned() {
    // `s.clone()` resolves to the `String.clone` intrinsic, whose
    // catalog mode (`Owned`) becomes the caller's.
    assert_eq!(
        function(&program(), "cloned").return_mode,
        ReturnMode::Owned
    );
}

#[test]
fn aliasing_intrinsic_call_threads_through_as_borrowed() {
    // `s.to_binary()` aliases `s`; the borrow propagates to the caller.
    assert_eq!(
        function(&program(), "as_bin").return_mode,
        ReturnMode::Borrowed
    );
}

#[test]
fn string_literal_is_static_borrowed() {
    assert_eq!(
        function(&program(), "literal_str").return_mode,
        ReturnMode::Borrowed,
    );
}

#[test]
fn move_param_threads_through_as_owned() {
    assert_eq!(
        function(&program(), "passed_through").return_mode,
        ReturnMode::Owned,
    );
}

#[test]
fn aggregate_over_borrowed_field_is_borrowed() {
    // `Opt.Some(w.value)` wraps a borrowed field — constructing the
    // enum does not mint ownership of the payload.
    assert_eq!(
        function(&program(), "wrap_field").return_mode,
        ReturnMode::Borrowed,
    );
}

#[test]
fn recursion_is_borrowed() {
    assert_eq!(
        function(&program(), "countdown").return_mode,
        ReturnMode::Borrowed,
    );
}
