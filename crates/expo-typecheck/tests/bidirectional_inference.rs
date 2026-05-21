//! Pin typecheck's bidirectional expected-type propagation:
//!
//! - The function-body trailing expression sees the declared return
//!   type as expected.
//! - `match` / `if` / `cond` / `ternary` arm tails inherit the same
//!   expected from the enclosing position.
//! - `return expr` threads the enclosing function's declared return
//!   type into `expr` so an early-return value gets the same
//!   bidirectional treatment as the trailing tail.
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
fn return_value_inherits_function_return_type() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn early_none<T>(flag: Bool) -> Maybe<T>
          if flag
            return Maybe.None
          end
          Maybe.None
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn return_inside_closure_uses_closure_return_type_not_outer_fn() {
    // The outer `fn` returns `Maybe<Int>`; the inner closure returns
    // `Maybe<String>`. The closure's `return Maybe.None` must pin the
    // unit variant against `Maybe<String>` (the closure's return),
    // not the outer fn's `Maybe<Int>`.
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        fn outer(flag: Bool) -> Maybe<Int>
          inner = fn (b: Bool) -> Maybe<String>
            if b
              return Maybe.None
            end
            Maybe.Some(\"x\")
          end
          inner(flag)
          Maybe.Some(1)
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
        failure.to_string().contains("cannot infer type parameter"),
        "expected `cannot infer type parameter` diagnostic; got {failure}",
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
        failure.to_string().contains("cannot infer type parameter"),
        "expected `cannot infer type parameter` diagnostic when expected type's \
         head differs from the unit variant's enum; got {failure}",
    );
}

// ------------------------------------------------------------------
// Sized-int widening through generic returns. When the surrounding
// expected type is a sized numeric primitive and the generic return
// would otherwise lock in as the default `Int` literal, the
// speculative pre-seed in `infer_call_type_args` keeps the sized
// slot intact via `Substitution::set`'s `literal_widens_into` rule.
// The literal arg picks up the matching `NumericLiteralWidth`
// coercion downstream.
// ------------------------------------------------------------------

#[test]
fn generic_return_hint_widens_int_literal_arg_to_int32() {
    let source = "
        fn identity<T>(x: T) -> T
          x
        end

        fn main
          x: Int32 = identity(42)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_return_hint_widens_int_literal_arg_to_int8() {
    let source = "
        fn identity<T>(x: T) -> T
          x
        end

        fn main
          x: Int8 = identity(7)
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_return_hint_unrelated_to_arg_type_still_errors() {
    use common::typecheck_file_fail as typecheck_fail;

    // `identity(42)` returns `Int`; annotating it as `String` must
    // still surface a diagnostic. The exact message may shift between
    // call-site (`T cannot be both`) and use-site (`expected String,
    // got Int`) depending on the speculative-pre-seed fallback path;
    // either is acceptable as long as SOME diagnostic fires.
    let source = "
        fn identity<T>(x: T) -> T
          x
        end

        fn main
          x: String = identity(42)
        end
        ";
    typecheck_fail(&dedent(source));
}

// ------------------------------------------------------------------
// Struct-field initializer positions get the declared field type as
// their expected hint, mirroring the function-tail / arm-tail
// propagation above. Pre-fix, `Option.None` and `[]` at field
// positions diagnosed because the per-field walk used bare
// `resolve_expr`.
// ------------------------------------------------------------------

#[test]
fn option_none_in_struct_field_infers_from_declared_type() {
    let source = "
        struct Diagnostic
          message: String
          hint: Option<String>
        end

        fn main
          d = Diagnostic{message: \"hi\", hint: Option.None}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn empty_list_in_struct_field_infers_from_declared_type() {
    let source = "
        struct Bag
          items: List<Int>
        end

        fn main
          b = Bag{items: []}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_struct_type_args_seed_field_inits_from_outer_annotation() {
    let source = "
        enum Maybe<T>
          Some(T)
          None
        end

        struct Bag<T>
          value: T
          alt: Maybe<T>
        end

        fn main
          b: Bag<Int> = Bag{value: 1, alt: Maybe.None}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn nested_struct_field_propagates_through_two_levels() {
    let source = "
        struct Inner
          deep: Option<String>
        end

        struct Outer
          inner: Inner
        end

        fn main
          o = Outer{inner: Inner{deep: Option.None}}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn option_none_in_enum_struct_variant_field_infers_from_declared_type() {
    let source = "
        enum Event
          Tick{at: Int, hint: Option<String>}
        end

        fn main
          e = Event.Tick{at: 1, hint: Option.None}
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn generic_return_hint_inside_unit_body_does_not_widen_unrelated_call() {
    // Regression for the `fn main` (Unit return) + trailing
    // `identity(1)` case: the outer expected hint is `Unit`, but
    // `T = Int` from the arg must win — the speculative pre-seed
    // would set `T = Unit` then conflict on the arg unify, so the
    // fallback path runs and the call types as `Int`.
    let source = "
        fn identity<T>(value: T) -> T
          value
        end

        fn main
          identity(1)
        end
        ";
    typecheck(&dedent(source));
}
