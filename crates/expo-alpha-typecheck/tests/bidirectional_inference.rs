//! Pin alpha typecheck's bidirectional expected-type propagation:
//!
//! - The function-body trailing expression sees the declared return
//!   type as expected.
//! - `match` / `if` / `cond` / `ternary` arm tails inherit the same
//!   expected from the enclosing position.
//! - Generic enum unit variants (`Option.None`) get their type args
//!   from the expected type when payload-driven inference can't
//!   constrain them.
//! - Generic enum tuple variants whose payload only constrains a
//!   subset of params (`Result.Ok(x)` pinning `T` but not `E`,
//!   `Result.Err(e)` pinning `E` but not `T`) fill the unconstrained
//!   slots from expected before the "cannot infer" diagnostic fires.
//!
//! The kernel.expo `Option.map` / `Result.map` shapes are the real-
//! world driver; these tests recreate them in a hermetic fixture so
//! a failure points squarely at the bidirectional plumbing.

use expo_ast::util::dedent;

mod common;

use common::typecheck_file as typecheck;

#[test]
fn unit_variant_inferred_from_function_return_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn empty<T>() -> Maybe<T>
          Maybe.None
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn unit_variant_inferred_from_match_arm_in_function_tail() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn map_to_string<T>(m: Maybe<T>) -> Maybe<String>
          match m
            Maybe.Some(_) -> Maybe.Some(\"x\")
            Maybe.None -> Maybe.None
          end
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn tuple_variant_partial_inference_filled_from_expected_first_param() {
    let source = "
        enum Outcome<T, E>
          Ok(T)
          Err(E)
        end

        fn always_err<T>(reason: String) -> Outcome<T, String>
          Outcome.Err(reason)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn tuple_variant_partial_inference_in_match_arm() {
    let source = "
        enum Outcome<T, E>
          Ok(T)
          Err(E)
        end

        fn map_ok<T, E, U>(x: Outcome<T, E>, val: U) -> Outcome<U, E>
          match x
            Outcome.Ok(_) -> Outcome.Ok(val)
            Outcome.Err(e) -> Outcome.Err(e)
          end
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn ternary_arm_propagates_expected_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn cond_or_none(flag: Bool, val: Int) -> Maybe<Int>
          flag ? Maybe.Some(val) : Maybe.None
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn if_else_arm_propagates_expected_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn pick_or_none(flag: Bool, val: Int) -> Maybe<Int>
          if flag
            Maybe.Some(val)
          else
            Maybe.None
          end
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn cond_arm_propagates_expected_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn first_match(a: Bool, b: Bool, val: Int) -> Maybe<Int>
          cond
            a -> Maybe.Some(val)
            b -> Maybe.Some(val)
            else -> Maybe.None
          end
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn nested_match_arm_inherits_outer_expected_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn collapse(outer: Maybe<Int>, inner: Maybe<Int>) -> Maybe<Int>
          match outer
            Maybe.None -> Maybe.None
            Maybe.Some(_) ->
              match inner
                Maybe.Some(v) -> Maybe.Some(v)
                Maybe.None -> Maybe.None
              end
          end
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn unit_variant_without_expected_still_diagnoses() {
    use common::typecheck_file_fail as typecheck_fail;

    // Bare assignment inside a Unit-returning function: the rhs
    // sits outside any expected-type position, so the unit-variant
    // diagnostic still fires (vs. the function-tail / arm-tail
    // shapes above where bidirectional inference fills the slot).
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn standalone()
          x = Maybe.None
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.to_string().contains("cannot infer type parameters"),
        "expected `cannot infer type parameters` diagnostic; got {failure}",
    );
}

#[test]
fn unit_variant_under_mismatched_expected_falls_back_to_diagnostic() {
    use common::typecheck_file_fail as typecheck_fail;

    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        enum Other<T>
          Just(T)
        end

        fn weird<T>() -> Other<T>
          Maybe.None
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.to_string().contains("cannot infer type parameters"),
        "expected `cannot infer type parameters` diagnostic when expected type's \
         head differs from the unit variant's enum; got {failure}",
    );
}
