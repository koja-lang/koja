//! End-to-end coverage for the interpreter dispatcher in
//! `src/interpreter.rs`. Drives `parse -> check -> lower ->
//! Interpreter::run_*` to observe the runtime [`Value`] each shape
//! produces.
//!
//! Three behavior areas live here, all hosted by the same
//! `execute_blocks` / `execute_function` / `execute_instruction` path:
//!
//! - Basic arithmetic + return-shape dispatch (project mode and
//!   script mode).
//! - `IRInstruction::Call` dispatch: zero-arg callee, multi-arg
//!   callee, nested calls in arithmetic, script-mode helper-fn
//!   resolution.
//! - `IRTerminator::CondBranch` dispatch: `if` and `unless` selecting
//!   between two arms at runtime, with helper functions whose `if` /
//!   `unless` cond is a literal Bool (identifier references inside
//!   bodies aren't resolved until the locals slice).
//!
//! Operator math (`apply_binary_op` / `apply_unary_op`) lives in
//! `tests/ops.rs`, paired with `src/ops.rs`.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

mod common;

use common::{evaluate_program as evaluate, evaluate_script};

// -- basic dispatch -------------------------------------------------

#[test]
fn fn_main_two_plus_two_evaluates_to_int_four() {
    assert_eq!(
        evaluate("fn main -> Int\n  2 + 2\nend\n").unwrap(),
        Value::Int(4),
    );
}

#[test]
fn bare_two_plus_two_script_evaluates_to_int_four() {
    assert_eq!(evaluate_script("2 + 2\n").unwrap(), Value::Int(4));
}

#[test]
fn integer_arithmetic_combinations() {
    assert_eq!(
        evaluate("fn main -> Int\n  10 - 3\nend\n").unwrap(),
        Value::Int(7),
    );
    assert_eq!(
        evaluate("fn main -> Int\n  6 * 7\nend\n").unwrap(),
        Value::Int(42),
    );
    assert_eq!(
        evaluate("fn main -> Int\n  20 / 4\nend\n").unwrap(),
        Value::Int(5),
    );
    assert_eq!(
        evaluate("fn main -> Int\n  17 % 5\nend\n").unwrap(),
        Value::Int(2),
    );
    assert_eq!(
        evaluate("fn main -> Int\n  (2 + 3) * 4\nend\n").unwrap(),
        Value::Int(20),
    );
}

#[test]
fn script_mode_arithmetic_matches_project_mode() {
    assert_eq!(evaluate_script("10 - 3\n").unwrap(), Value::Int(7));
    assert_eq!(evaluate_script("(2 + 3) * 4\n").unwrap(), Value::Int(20));
}

#[test]
fn division_by_zero_panics() {
    let err = evaluate("fn main -> Int\n  10 / 0\nend\n").expect_err("should fail at runtime");
    assert_eq!(
        err,
        RuntimeError::Panicked {
            message: "division by zero in /".to_string(),
        },
    );
}

#[test]
fn empty_main_returns_unit() {
    assert_eq!(evaluate("fn main\nend\n").unwrap(), Value::Unit,);
}

// -- string literals -----------------------------------------------

#[test]
fn string_literal_evaluates_to_value_string() {
    let source = "fn main -> String\n  \"hello\"\nend\n";
    assert_eq!(evaluate(source).unwrap(), Value::string("hello"),);
}

#[test]
fn string_literal_in_script_mode_evaluates_to_value_string() {
    assert_eq!(
        evaluate_script("\"hello\"\n").unwrap(),
        Value::string("hello"),
    );
}

#[test]
fn empty_string_literal_evaluates_to_empty_value_string() {
    assert_eq!(
        evaluate("fn main -> String\n  \"\"\nend\n").unwrap(),
        Value::string(Vec::new()),
    );
}

#[test]
fn value_string_displays_without_quotes() {
    let value = Value::string("hello");
    assert_eq!(format!("{value}"), "hello");
}

// -- Call instruction dispatch -------------------------------------

#[test]
fn zero_arg_call_returns_callee_value() {
    let source = "
        fn answer -> Int
          42
        end

        fn main -> Int
          answer() + 1
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(43));
}

#[test]
fn nested_zero_arg_calls_combine_via_arithmetic() {
    let source = "
        fn a -> Int
          1
        end

        fn b -> Int
          2
        end

        fn main -> Int
          a() + b()
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(3));
}

#[test]
fn arg_taking_callee_with_unreferenced_param_returns_body_value() {
    // Function bodies cannot reference parameters yet (typecheck
    // would reject `x`). This test stakes ground for arity
    // correctness: the arg is evaluated and bound in the callee's
    // frame even though the body never reads it, and the call
    // returns the body's constant.
    let source = "
        fn take(x: Int) -> Int
          7
        end

        fn main -> Int
          take(99)
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(7));
}

#[test]
fn multiple_args_evaluate_in_order() {
    // Same param-reference limitation applies: neither param is
    // referenced, so the return is a constant. The value carries
    // out the multi-arg call path end-to-end.
    let source = "
        fn pair(a: Int, b: Int) -> Int
          11
        end

        fn main -> Int
          pair(2, 3)
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(11));
}

#[test]
fn call_return_participates_in_outer_expression() {
    let source = "
        fn double -> Int
          4
        end

        fn main -> Int
          double() * 5
        end
        ";

    let program = dedent(source);
    assert_eq!(evaluate(&program).unwrap(), Value::Int(20));
}

#[test]
fn script_body_calls_helper_fn_in_packages() {
    // Mirror of `zero_arg_call_returns_callee_value` for script
    // mode: the helper fn lives in the script's package fragment;
    // the implicit body calls it. Drives `lower_script` +
    // `Interpreter::run_script` end to end.
    let source = "
        fn answer -> Int
          42
        end

        answer() + 1
        ";

    let script = dedent(source);
    assert_eq!(evaluate_script(&script).unwrap(), Value::Int(43));
}

// -- CondBranch terminator dispatch --------------------------------

#[test]
fn if_with_true_condition_executes_then_branch() {
    // The early `return 1` inside the `if true` body fires; the
    // merge block's trailing `2` is unreachable.
    let source = "
        fn pick -> Int
          if true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn if_with_false_condition_falls_through_to_merge() {
    // The cond evaluates to `false`, so the then-block is skipped
    // entirely; the trailing `2` in the merge block is the
    // function's return value.
    let source = "
        fn pick -> Int
          if false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn unless_with_false_condition_executes_body() {
    // `unless cond` runs the body when cond is `false`. The early
    // `return 1` therefore fires when the cond is the literal
    // `false`.
    let source = "
        fn pick -> Int
          unless false
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn unless_with_true_condition_skips_body() {
    let source = "
        fn pick -> Int
          unless true
            return 1
          end
          2
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn if_drives_program_mode_through_helper_calls() {
    // Mirror of the script-mode coverage above, exercising the
    // project-mode entry path. Each helper exercises a different
    // arm of the same `if` shape and `main` sums them.
    let source = "
        fn pick_then -> Int
          if true
            return 1
          end
          2
        end

        fn pick_merge -> Int
          if false
            return 1
          end
          2
        end

        fn main -> Int
          pick_then() + pick_merge()
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(3));
}

// -- value-producing if/else (block params) -------------------------

#[test]
fn if_else_value_producing_then_arm_with_true_condition() {
    // The if/else lowers to a 4-block CFG with a typed BlockParam
    // on the merge; cond=true reaches merge with the then-arm's
    // value, which becomes the function's return.
    let source = "
        fn pick -> Int
          if true
            7
          else
            9
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn if_else_value_producing_else_arm_with_false_condition() {
    let source = "
        fn pick -> Int
          if false
            7
          else
            9
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(9));
}

#[test]
fn if_else_with_diverging_then_arm_produces_else_value() {
    // The then-arm diverges via `return`; only the else-arm reaches
    // merge, passing its tail value via the BlockParam.
    let source = "
        fn pick -> Int
          if false
            return 1
          else
            42
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(42));
}

// -- cond -----------------------------------------------------------

#[test]
fn cond_first_matching_arm_drives_return() {
    let source = "
        fn pick -> Int
          cond
            true -> 1
            false -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn cond_second_arm_runs_when_first_test_is_false() {
    let source = "
        fn pick -> Int
          cond
            false -> 1
            true -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn cond_else_runs_when_no_arm_matches() {
    let source = "
        fn pick -> Int
          cond
            false -> 1
            false -> 2
            else -> 3
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(3));
}

// -- ternary --------------------------------------------------------

#[test]
fn ternary_returns_then_value_when_true() {
    let source = "
        fn pick -> Int
          true ? 7 : 9
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn ternary_returns_else_value_when_false() {
    let source = "
        fn pick -> Int
          false ? 7 : 9
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(9));
}

// -- match ----------------------------------------------------------

#[test]
fn match_int_literal_first_arm_wins_when_subject_matches() {
    let source = "
        fn pick -> Int
          match 1
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(10));
}

#[test]
fn match_int_literal_falls_through_to_second_arm() {
    let source = "
        fn pick -> Int
          match 2
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(20));
}

#[test]
fn match_wildcard_catch_all_runs_when_no_literal_matches() {
    let source = "
        fn pick -> Int
          match 99
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(30));
}

#[test]
fn match_binding_arm_evaluates_with_subject_bound_to_name() {
    let source = "
        fn pick -> Int
          match 7
            x -> x + 1
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(8));
}

#[test]
fn match_string_literal_first_arm_wins_on_string_equality() {
    let source = "
        fn pick -> Int
          match \"hi\"
            \"hi\" -> 1
            _ -> 0
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn match_string_literal_falls_through_when_subject_differs() {
    let source = "
        fn pick -> Int
          match \"world\"
            \"hi\" -> 1
            _ -> 0
          end
        end

        pick()
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(0));
}

#[test]
fn match_enum_tuple_some_binds_payload_to_local() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) -> x
            Box.None -> 0
          end
        end

        unwrap(Box.Some(7))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn match_enum_tuple_none_falls_through_to_unit_arm() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) -> x
            Box.None -> 99
          end
        end

        unwrap(Box.None)
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(99));
}

#[test]
fn match_result_ok_arm_returns_ok_payload() {
    let source = "
        enum Status
          Ok(Int)
          Err(Int)
        end

        fn classify(s: Status) -> Int
          match s
            Status.Ok(v) -> v
            Status.Err(_) -> -1
          end
        end

        classify(Status.Ok(42))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(42));
}

#[test]
fn match_result_err_arm_runs_when_payload_is_err() {
    let source = "
        enum Status
          Ok(Int)
          Err(Int)
        end

        fn classify(s: Status) -> Int
          match s
            Status.Ok(v) -> v
            Status.Err(_) -> -1
          end
        end

        classify(Status.Err(13))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(-1));
}

#[test]
fn match_or_of_strings_fires_for_any_alternative() {
    let source = "
        fn pick(s: String) -> Int
          match s
            \"a\" | \"b\" | \"c\" -> 1
            _ -> 0
          end
        end

        pick(\"b\")
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(1));
}

#[test]
fn match_guarded_arm_fires_when_guard_is_true() {
    let source = "
        fn pick(n: Int) -> Int
          match n
            x when x > 0 -> 10
            _ -> 20
          end
        end

        pick(7)
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(10));
}

#[test]
fn match_guarded_arm_falls_through_when_guard_is_false() {
    let source = "
        fn pick(n: Int) -> Int
          match n
            x when x > 0 -> 10
            _ -> 20
          end
        end

        pick(-3)
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(20));
}

#[test]
fn match_guarded_enum_payload_binding_visible_to_guard() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) when x > 0 -> x
            _ -> -1
          end
        end

        unwrap(Box.Some(7))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn match_guarded_enum_payload_arm_falls_through_on_guard_false() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) when x > 0 -> x
            _ -> -1
          end
        end

        unwrap(Box.Some(-4))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(-1));
}

#[test]
fn match_struct_destructure_binds_each_field() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn add(p: Point) -> Int
          match p
            Point{x: a, y: b} -> a + b
          end
        end

        add(Point{x: 3, y: 4})
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(7));
}

#[test]
fn match_struct_destructure_partial_omits_unlisted_fields() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn x_only(p: Point) -> Int
          match p
            Point{x: x} -> x
          end
        end

        x_only(Point{x: 5, y: 99})
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(5));
}

#[test]
fn match_struct_destructure_with_wildcard_field_skips_bind() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn first(p: Point) -> Int
          match p
            Point{x: a, y: _} -> a
          end
        end

        first(Point{x: 9, y: 4})
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(9));
}

#[test]
fn match_enum_struct_destructure_dispatches_by_variant() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle{r: Int}
        end

        fn area(s: Shape) -> Int
          match s
            Shape.Rect{w: w, h: h} -> w * h
            Shape.Circle{r: r} -> r * r
          end
        end

        area(Shape.Rect{w: 3, h: 4}) + area(Shape.Circle{r: 5})
        ";
    assert_eq!(
        evaluate_script(&dedent(source)).unwrap(),
        Value::Int(12 + 25)
    );
}

#[test]
fn match_enum_struct_destructure_visible_to_guard() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle{r: Int}
        end

        fn classify(s: Shape) -> Int
          match s
            Shape.Rect{w: w, h: h} when w == h -> 1
            Shape.Rect{w: _, h: _} -> 2
            Shape.Circle{r: _} -> 3
          end
        end

        classify(Shape.Rect{w: 4, h: 4}) + classify(Shape.Rect{w: 3, h: 4}) + classify(Shape.Circle{r: 9})
        ";
    assert_eq!(
        evaluate_script(&dedent(source)).unwrap(),
        Value::Int(1 + 2 + 3)
    );
}

#[test]
fn match_exhaustive_enum_no_catch_all_runs_correctly() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        fn rank(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
            Color.Blue -> 3
          end
        end

        rank(Color.Green)
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(2));
}

#[test]
fn match_unguarded_catch_all_skips_lowering_of_following_arms() {
    let source = "
        enum Color
          Red
          Blue
          Green
        end

        c = Color.Blue

        match c
          _ -> \"catchall\"
          Color.Green -> \"green\"
        end
        ";
    assert_eq!(
        evaluate_script(&dedent(source)).unwrap(),
        Value::string("catchall"),
    );
}

#[test]
fn match_constructor_some_binds_inner_value() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Some(x) -> x + 1
            None -> 0
          end
        end

        unwrap(Box.Some(7))
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(8));
}

#[test]
fn match_constructor_none_arm_runs_on_none_subject() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Some(x) -> x
            None -> 99
          end
        end

        unwrap(Box.None)
        ";
    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Int(99));
}
