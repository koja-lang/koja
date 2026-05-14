//! Runtime coverage for match arms with literal / partial / nested
//! patterns inside struct fields and enum tuple payloads. Mirrors
//! the five `tests/lang/types/` fixtures (`struct_pattern_*`,
//! `nested_enum_pattern_literal`) at the eval layer so a regression
//! in [`expo_alpha_ir::lower::patterns`] (AND-chain wiring,
//! payload-projection ordering, chained-bind extraction) fires
//! here before the lang golden suite ever runs.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

fn evaluate_program(source: &str) -> Value {
    common::evaluate_program(source).expect("interpreter should not error on this fixture")
}

fn assert_string(value: Value, expected: &str) {
    let Value::String(actual) = value else {
        panic!("expected Value::String, got {value:?}");
    };
    assert_eq!(actual.as_slice(), expected.as_bytes());
}

#[test]
fn struct_pattern_basic_literal_arms_select_exact_match() {
    // Mirrors `tests/lang/types/struct_pattern_basic.expo`'s first
    // call: `Point{x: 5, y: 2}` must hit the `Point{x: 5, y: 2}`
    // arm and return "exact".
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> String
          match p
            Point{x: 5, y: 2} -> \"exact\"
            Point{x: 0, y: 0} -> \"origin\"
            _ -> \"other\"
          end
        end

        fn main -> String
          classify(Point{x: 5, y: 2})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "exact");
}

#[test]
fn struct_pattern_basic_second_literal_arm_is_reachable() {
    // The chained AND wiring must fall through to the second arm
    // when the first arm's tests fail. Pin both per-arm hits and
    // the final catch-all.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> String
          match p
            Point{x: 5, y: 2} -> \"exact\"
            Point{x: 0, y: 0} -> \"origin\"
            _ -> \"other\"
          end
        end

        fn main -> String
          classify(Point{x: 0, y: 0})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "origin");
}

#[test]
fn struct_pattern_basic_partial_match_on_first_field_falls_through() {
    // `Point{x: 5, y: 9}` matches `x` in the first arm but fails
    // on `y`; must fall through to the catch-all.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> String
          match p
            Point{x: 5, y: 2} -> \"exact\"
            Point{x: 0, y: 0} -> \"origin\"
            _ -> \"other\"
          end
        end

        fn main -> String
          classify(Point{x: 5, y: 9})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "other");
}

#[test]
fn struct_pattern_bind_extracts_named_fields_into_locals() {
    // Pin chained-bind correctness via field interpolation: the
    // bound `x` and `y` locals must hold the right field values.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn describe(p: Point) -> String
          match p
            Point{x: 0, y: 0} -> \"at origin\"
            Point{x: x, y: y} -> \"x=#{x}, y=#{y}\"
          end
        end

        fn main -> String
          describe(Point{x: 3, y: 4})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "x=3, y=4");
}

#[test]
fn struct_pattern_partial_omitted_field_is_implicit_wildcard() {
    // `Point{x: 5}` matches when `x == 5` regardless of `y`. Pin
    // both the partial-match success path and the empty `Point{}`
    // catch-all.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> String
          match p
            Point{x: 5} -> \"x is five\"
            Point{} -> \"any point\"
          end
        end

        fn main -> String
          classify(Point{x: 5, y: 99})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "x is five");
}

#[test]
fn struct_pattern_partial_empty_destructure_acts_as_catch_all() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> String
          match p
            Point{x: 5} -> \"x is five\"
            Point{} -> \"any point\"
          end
        end

        fn main -> String
          classify(Point{x: 1, y: 2})
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "any point");
}

#[test]
fn struct_pattern_nested_inside_option_some_matches_inner_literal() {
    // `Option.Some(Point{x: 0, y: 0})` exercises tag-then-payload
    // CFG ordering: the inner struct fields must only be inspected
    // when the Option tag check succeeded.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn label(op: Option<Point>) -> String
          match op
            Option.Some(Point{x: 0, y: 0}) -> \"some origin\"
            Option.Some(Point{x: 5}) -> \"some x=5\"
            Option.Some(Point{x: x, y: y}) -> \"some (#{x}, #{y})\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          label(Option.Some(Point{x: 0, y: 0}))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "some origin");
}

#[test]
fn struct_pattern_nested_falls_through_to_partial_then_catch_all() {
    // `Option.Some(Point{x: 5, y: 1})` should miss the origin arm
    // and hit the `Point{x: 5}` partial pattern.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn label(op: Option<Point>) -> String
          match op
            Option.Some(Point{x: 0, y: 0}) -> \"some origin\"
            Option.Some(Point{x: 5}) -> \"some x=5\"
            Option.Some(Point{x: x, y: y}) -> \"some (#{x}, #{y})\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          label(Option.Some(Point{x: 5, y: 9}))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "some x=5");
}

#[test]
fn struct_pattern_nested_extracts_inner_struct_via_chained_bind() {
    // The full binding arm exercises the chained-bind path:
    // EnumPayloadFieldGet (Option.Some payload) → FieldGet
    // (Point.x and Point.y) → LocalWrite per field.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn label(op: Option<Point>) -> String
          match op
            Option.Some(Point{x: 0, y: 0}) -> \"some origin\"
            Option.Some(Point{x: 5}) -> \"some x=5\"
            Option.Some(Point{x: x, y: y}) -> \"some (#{x}, #{y})\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          label(Option.Some(Point{x: 7, y: 8}))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "some (7, 8)");
}

#[test]
fn struct_pattern_nested_option_none_does_not_segfault_on_payload_read() {
    // Phase 4g regression: None must not trigger a payload
    // projection in any of the Some-shaped arms.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn label(op: Option<Point>) -> String
          match op
            Option.Some(Point{x: 0, y: 0}) -> \"some origin\"
            Option.Some(Point{x: 5}) -> \"some x=5\"
            Option.Some(Point{x: x, y: y}) -> \"some (#{x}, #{y})\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          op: Option<Point> = Option.None
          label(op)
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "none");
}

#[test]
fn nested_enum_pattern_literal_matches_inner_string_literal() {
    // `Option.Some(TokenKind.Ident(\"and\"))` exercises three
    // levels of nesting: outer Option tag, inner TokenKind tag,
    // innermost String literal payload.
    let source = "
        enum TokenKind
          Ident(String)
          Number(Int)
        end

        fn classify(opt: Option<TokenKind>) -> String
          match opt
            Option.Some(TokenKind.Ident(\"and\")) -> \"matched and\"
            Option.Some(TokenKind.Ident(name)) -> name
            Option.Some(TokenKind.Number(0)) -> \"zero\"
            Option.Some(TokenKind.Number(_n)) -> \"other number\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          classify(Option.Some(TokenKind.Ident(\"and\")))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "matched and");
}

#[test]
fn nested_enum_pattern_falls_through_to_inner_binding_arm() {
    let source = "
        enum TokenKind
          Ident(String)
          Number(Int)
        end

        fn classify(opt: Option<TokenKind>) -> String
          match opt
            Option.Some(TokenKind.Ident(\"and\")) -> \"matched and\"
            Option.Some(TokenKind.Ident(name)) -> name
            Option.Some(TokenKind.Number(0)) -> \"zero\"
            Option.Some(TokenKind.Number(_n)) -> \"other number\"
            Option.None -> \"none\"
          end
        end

        fn main -> String
          classify(Option.Some(TokenKind.Ident(\"xyz\")))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "xyz");
}

#[test]
fn multi_arg_tuple_variant_with_literal_short_circuits_on_first_slot() {
    // `IntPair.Just(0, _)`: the second-slot projection must only
    // run when the first-slot literal compare succeeds. Pin both
    // the match and the fall-through.
    let source = "
        enum IntPair
          Just(Int, Int)
          Nope
        end

        fn pair_kind(p: IntPair) -> String
          match p
            IntPair.Just(0, _) -> \"starts zero\"
            IntPair.Just(1, 2) -> \"one two\"
            IntPair.Just(_a, _b) -> \"other\"
            IntPair.Nope -> \"nope\"
          end
        end

        fn main -> String
          pair_kind(IntPair.Just(0, 99))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "starts zero");
}

#[test]
fn multi_arg_tuple_variant_matches_both_literal_payload_slots() {
    let source = "
        enum IntPair
          Just(Int, Int)
          Nope
        end

        fn pair_kind(p: IntPair) -> String
          match p
            IntPair.Just(0, _) -> \"starts zero\"
            IntPair.Just(1, 2) -> \"one two\"
            IntPair.Just(_a, _b) -> \"other\"
            IntPair.Nope -> \"nope\"
          end
        end

        fn main -> String
          pair_kind(IntPair.Just(1, 2))
        end
        ";
    assert_string(evaluate_program(&dedent(source)), "one two");
}
