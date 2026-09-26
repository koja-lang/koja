mod common;

use common::*;

#[test]
fn list_comments_stay_with_elements() {
    assert_fmt_script(
        "
            list = [
              # first element
              1,
              2, # trailing
              3,
            ]
        ",
        "
            list = [
              # first element
              1, 2, # trailing
              3
            ]
        ",
    );
}

#[test]
fn list_packing_resumes_around_comments() {
    assert_unchanged_script(
        "
            big = [
              1, 2, # two
              3, 4, 5, 6,
              # header values start here
              7, 8
            ]
        ",
    );
}

#[test]
fn list_comment_before_closing_bracket_stays_inside() {
    assert_unchanged_script(
        "
            closed = [
              1, 2
              # before the bracket
            ]
        ",
    );
}

#[test]
fn map_entry_trailing_comment_stays_with_entry() {
    assert_unchanged_script(
        "
            m = [
              \"a\": 1, # first
              \"b\": 2
            ]
        ",
    );
}

#[test]
fn binary_literal_comment_breaks_its_line() {
    assert_unchanged_script(
        "
            b = <<
              1, 2, # second
              3
            >>
        ",
    );
}

#[test]
fn construction_field_comments_stay_with_fields() {
    assert_unchanged_script(
        "
            p = Point{
              # horizontal
              x: 1,
              y: 2, # vertical
            }
        ",
    );
}

#[test]
fn call_arg_comments_stay_with_args() {
    assert_unchanged_script(
        "
            r = compute(
              # first arg
              2,
              3, # second
            )
        ",
    );
}

#[test]
fn param_comments_force_broken_signature() {
    assert_unchanged(
        "
            fn compute(
              # the base value
              base: Int32,
              scale: Int32, # multiplier
            ) -> Int32

              base * scale
            end
        ",
    );
}

#[test]
fn chain_comment_anchors_to_its_link() {
    // A comment inside an assigned chain forces the break after `=`.
    assert_unchanged_script(
        "
            out =
              [3, 1, 2]
              .map(v -> v * 2)
              # drop the small ones
              .filter(v -> v > 2)
        ",
    );
}

#[test]
fn struct_literal_field_trailing_comment_survives() {
    assert_unchanged(
        "
            fn f -> Point
              Point{
                x: 1, # horizontal
                y: 2,
              }
            end
        ",
    );
}

#[test]
fn enum_struct_literal_field_trailing_comment_survives() {
    assert_unchanged(
        "
            fn f -> Shape
              Shape.Rect{
                width: 1, # px
                height: 2,
              }
            end
        ",
    );
}

#[test]
fn chain_statement_leading_comment_stays_above_statement() {
    assert_fmt(
        r#"
            fn f(code: Int32) -> String
              match code
                1 ->
                  # wrap and copy, never free
                  error_string(code).to_cstring().to_string().unwrap()
                _ -> "other"
              end
            end
        "#,
        r#"
            fn f(code: Int32) -> String
              match code
                1 ->
                  # wrap and copy, never free
                  error_string(code).to_cstring().to_string().unwrap()

                _ ->
                  "other"
              end
            end
        "#,
    );
}

#[test]
fn broken_assignment_head_comment_hoists_above_statement() {
    assert_fmt(
        r#"
            fn f -> String
              r = # note
                match 1
                  _ -> "x"
                end
              r
            end
        "#,
        r#"
            fn f -> String
              # note
              r =
                match 1
                  _ -> "x"
                end

              r
            end
        "#,
    );
}
