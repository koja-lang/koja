//! Typecheck coverage for `while`, `loop`, and `for`.
//!
//! `while` pins: `Bool` condition, body resolves under the enclosing
//! scope, mutable bindings propagate, expression resolves to `Unit`.
//!
//! `loop` pins the `Never`-vs-`Unit` typing rule: a body with no
//! targeted `break` is divergent (`Never`), one with at least one
//! `break` yields `Unit`. `break` is gated on `loop_depth > 0` and
//! marks the innermost loop's `loop_break_seen` slot. Closure
//! boundaries reset both fields, so an inner closure's `break`
//! can't reach an outer-function loop.
//!
//! Statement-position `for pat in source ... end` rewrites during
//! resolve after the source type proves nominal `Enumeration<T,
//! Cursor>` conformance. The generated loop advances its cursor
//! before the surface body.

use koja_ast::ast::{Expr, ExprKind, Statement};
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, int_type, never_type, trailing_expr,
    trailing_resolution, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
    unit_type,
};

/// Cursor-based `Enumeration<Int, Int>` fixture.
const ENUMERABLE_FIXTURE: &str = "
    struct Counter
      start: Int
      finish: Int
    end

    impl Enumeration<Int, Int> for Counter
      fn cursor(self) -> Int
        self.start
      end

      fn next(self, cursor: Int) -> Option<(Int, Int)>
        if cursor < self.finish
          Option.Some((cursor, cursor + 1))
        else
          Option.None
        end
      end
    end
    ";

/// Trailing `loop`'s body's first/only `Statement::Expr` payload.
/// The nested-break test runs against this to inspect an inner
/// loop's resolution.
fn trailing_loop_inner_expr(checked: &CheckedProgram) -> &Expr {
    let trailing = trailing_expr(checked);
    let ExprKind::Loop { body } = &trailing.kind else {
        panic!("expected trailing ExprKind::Loop, got {:?}", trailing.kind);
    };
    let Some(Statement::Expr(inner)) = body.first() else {
        panic!("expected loop body to start with Statement::Expr, got {body:?}");
    };
    inner
}

#[test]
fn while_with_bool_condition_resolves_to_unit() {
    let source = "
        i = 0
        while i < 3
          i = i + 1
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn while_with_int_condition_diagnoses() {
    let source = "
        while 1
          2
        end
        ";
    assert_script_fails_with(source, &["`while` condition must be `Bool`"]);
}

#[test]
fn while_body_assignment_propagates_local_type() {
    // Mutable bindings inside the body must resolve through the
    // same `LocalScope::declare` path as anywhere else. Subsequent
    // reads see the same `LocalId`.
    let source = "
        i = 0
        sum = 0
        while i < 10
          sum = sum + i
          i = i + 1
        end
        sum
        ";
    let checked = typecheck(&dedent(source));
    // Trailing `sum` reads the body-mutated local. Its resolution
    // is `Int`, proving the body's writes propagated.
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn while_with_string_condition_diagnoses() {
    let source = "
        while \"yes\"
          1
        end
        ";
    assert_script_fails_with(source, &["`while` condition must be `Bool`"]);
}

fn with_fixture(body: &str) -> String {
    format!("{ENUMERABLE_FIXTURE}\n{body}")
}

#[test]
fn for_over_enumerable_resolves_to_unit_and_binds_int() {
    // The Some-arm binds `x: Int`, so the body's `sum + x`
    // typechecks. Trailing `sum` proves the binding flowed.
    let source = with_fixture(
        "
        c = Counter{start: 10, finish: 13}
        sum = 0
        for x in c
          sum = sum + x
        end
        sum
        ",
    );
    let checked = typecheck(&dedent(&source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn for_with_wildcard_pattern_typechecks() {
    // `_` skips binding. The body still needs to resolve, but
    // there's no binding to consult.
    let source = with_fixture(
        "
        c = Counter{start: 0, finish: 5}
        count = 0
        for _ in c
          count = count + 1
        end
        count
        ",
    );
    let checked = typecheck(&dedent(&source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn for_over_int_requires_enumeration_conformance() {
    let source = "
        for x in 5
          x
        end
        ";
    assert_script_fails_with(
        source,
        &["type `Int` in a `for` loop must implement `Enumeration<T, Cursor>`"],
    );
}

#[test]
fn for_over_unrelated_struct_requires_enumeration_conformance() {
    let source = "
        struct Bare
          x: Int
        end

        b = Bare{x: 1}
        for v in b
          v
        end
        ";
    assert_script_fails_with(
        source,
        &["type `Bare` in a `for` loop must implement `Enumeration<T, Cursor>`"],
    );
}

#[test]
fn for_rejects_refutable_header_pattern() {
    let source = "
        for 1 in [1, 2]
          1
        end
        ";
    assert_script_fails_with(
        source,
        &["`for` requires an irrefutable pattern. The header contains a literal pattern."],
    );
}

#[test]
fn for_requires_nominal_conformance_even_with_matching_functions() {
    let source = "
        struct Structural
        end

        extend Structural
          fn cursor(self) -> Int
            0
          end

          fn next(self, cursor: Int) -> Option<(Int, Int)>
            Option.None
          end
        end

        for value in Structural{}
          value
        end
        ";
    assert_script_fails_with(
        source,
        &["type `Structural` in a `for` loop must implement `Enumeration<T, Cursor>`"],
    );
}

#[test]
fn for_accepts_parameter_with_enumeration_bound() {
    let source = with_fixture(
        "
        fn sum<T: Enumeration<Int, Int>>(source: T) -> Int
          total = 0
          for value in source
            total = total + value
          end
          total
        end

        sum(Counter{start: 1, finish: 4})
        ",
    );
    let checked = typecheck(&dedent(&source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn for_rejects_parameter_without_enumeration_bound() {
    let source = "
        fn consume<T>(source: T) -> Unit
          for value in source
            value
          end
        end

        consume(1)
        ";
    assert_script_fails_with(
        source,
        &["type `T` in a `for` loop must implement `Enumeration<T, Cursor>`"],
    );
}

#[test]
fn expression_position_for_stays_unsupported() {
    let source = with_fixture(
        "
        c = Counter{start: 0, finish: 1}
        value = (for item in c
          item
        end)
        value
        ",
    );
    assert_script_fails_with(
        &source,
        &["typecheck does not yet support `for` in expression position"],
    );
}

#[test]
fn for_with_nested_tuple_pattern_typechecks() {
    let source = "
        pairs = [(1, (2, 3))]
        total = 0
        for (a, (b, c)) in pairs
          total = a + b + c
        end
        total
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn loop_with_no_break_resolves_to_never() {
    // Body has no `break`, so the loop is divergent and types as
    // `Never`. The function returns `Never`-shorted by the existing
    // `check_return_type` short-circuit.
    let source = "
        loop
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), never_type(&checked));
}

#[test]
fn loop_with_break_resolves_to_unit() {
    // A reachable `break` flips the loop's type to `Unit`, the
    // value the loop yields when control exits at the break.
    let source = "
        loop
          break
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn loop_with_only_inner_return_resolves_to_never() {
    // Body's only "exit" is a nested bare `return` (no `break`), so
    // the loop stays `Never`. Bare, since a valued `return` in a
    // script body is rejected.
    let source = "
        loop
          return
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), never_type(&checked));
}

#[test]
fn fn_int_loop_with_break_diagnoses_unit_int_mismatch() {
    // The loop with a reachable `break` types as `Unit`, which
    // doesn't match the declared `-> Int`. The conservative-but-
    // sound win over typing `loop` as always-`Never`: nothing in
    // this function actually produces an `Int`.
    let source = "
        fn run -> Int
          loop
            break
          end
        end
        run()
        ";
    assert_script_fails_with(source, &["return type"]);
}

#[test]
fn break_inside_while_typechecks() {
    // `while` also bumps `loop_depth`, so a break in its body is
    // admitted. `while` keeps its `Unit` return type regardless.
    let source = "
        while true
          break
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn nested_break_marks_only_inner_loop() {
    // `loop loop break end end`: the inner loop's break flips the
    // *inner* `loop_break_seen` slot, so the inner loop resolves
    // `Unit` and the outer loop's slot stays `false`, so the outer
    // resolves `Never`.
    let source = "
        loop
          loop
            break
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), never_type(&checked));
    let inner = trailing_loop_inner_expr(&checked);
    assert_eq!(inner.resolution, unit_type(&checked));
}

#[test]
fn break_outside_loop_diagnoses() {
    // `break` at function-body top level has no enclosing loop, so
    // `loop_depth == 0` triggers the diagnostic.
    let source = "
        break
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m == "break outside of loop"),
        "expected `break outside of loop` diagnostic, got: {messages:?}",
    );
}

#[test]
fn break_inside_closure_inside_loop_diagnoses_and_outer_loop_stays_never() {
    // A `break` inside a closure body must reference a loop *inside*
    // the closure. The closure boundary resets `loop_depth` to 0, so
    // this break diagnoses. The outer loop's `loop_break_seen` slot
    // is untouched, so the outer loop still resolves `Never`. Pins
    // both the gate and the closure-boundary reset.
    let source = "
        loop
          f = fn () -> Unit
            break
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m == "break outside of loop"),
        "expected `break outside of loop` diagnostic, got: {messages:?}",
    );
}
