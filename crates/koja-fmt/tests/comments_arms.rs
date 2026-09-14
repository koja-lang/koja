mod common;

use common::*;

#[test]
fn match_arm_trailing_comment_stays_on_arm() {
    assert_unchanged(
        "
            fn f(x: Int) -> Int
              match x
                1 -> 10 # one
                _ -> 0
              end
            end
        ",
    );
}

#[test]
fn adjacent_comment_between_match_arms_stays_with_previous_arm() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                # everything else
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 ->
                  10
                  # everything else

                _ ->
                  0
              end
            end
        ",
    );
}

#[test]
fn blank_line_controls_arm_boundary_comment_ownership() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 ->
                  10
                  # stays inside the first arm

                # leads the wildcard arm
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 ->
                  10
                  # stays inside the first arm

                # leads the wildcard arm
                _ ->
                  0
              end
            end
        ",
    );
}

#[test]
fn cond_arm_trailing_comment_stays_on_arm() {
    assert_unchanged(
        "
            fn f(x: Int) -> Int
              cond
                x > 10 -> 1 # big
                else -> 0
              end
            end
        ",
    );
}

#[test]
fn receive_arm_trailing_comment_stays_on_arm() {
    assert_unchanged(
        "
            fn f -> Int
              receive
                n: Int -> n # next job
              end
            end
        ",
    );
}

#[test]
fn arm_head_trailing_comment_forces_broken_body() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 -> # one
                  10
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 -> # one
                  10

                _ ->
                  0
              end
            end
        ",
    );
}

#[test]
fn or_pattern_comment_hoists_above_match_arm() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 |
                # grouped cases
                2 -> # selected
                  10
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                # grouped cases
                1 | 2 -> # selected
                  10

                _ ->
                  0
              end
            end
        ",
    );
}

#[test]
fn wrapped_cond_head_comment_hoists_above_arm() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              cond
                x > 0
                # bounded positive
                and x < 100 -> # selected
                  1
                else -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              cond
                # bounded positive
                x > 0 and x < 100 -> # selected
                  1

                else ->
                  0
              end
            end
        ",
    );
}

#[test]
fn wrapped_receive_guard_comment_hoists_above_arm() {
    assert_fmt(
        "
            fn f -> Int
              receive
                n: Int when n > 0
                # bounded positive
                and n < 100 -> # selected
                  n
              end
            end
        ",
        "
            fn f -> Int
              receive
                # bounded positive
                n: Int when n > 0 and n < 100 -> # selected
                  n
              end
            end
        ",
    );
}

#[test]
fn arm_body_leading_comment_forces_broken_body() {
    assert_fmt(
        "
            fn f(x: Int) -> Bool
              match x
                1 -> true # awesome
                _ ->
                  # kinda meh
                  false
              end
            end
        ",
        "
            fn f(x: Int) -> Bool
              match x
                1 ->
                  true # awesome

                _ ->
                  # kinda meh
                  false
              end
            end
        ",
    );
}

#[test]
fn collapsed_arm_keeps_body_trailing_comment() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 ->
                  10 # one
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 -> 10 # one
                _ -> 0
              end
            end
        ",
    );
}

#[test]
fn else_body_leading_comment_stays_in_else() {
    assert_unchanged(
        r#"
            fn f(n: Int32)
              if n > 2
                "big".print()
              else
                # leading comment in else body
                "small".print()
              end
            end
        "#,
    );
}

#[test]
fn comment_before_else_stays_above_else() {
    assert_unchanged(
        r#"
            fn f(n: Int32)
              if n > 2
                "big".print()
              # before else
              else # on else line
                "small".print()
              end
            end
        "#,
    );
}

#[test]
fn cond_comment_before_else_stays_above_else() {
    assert_fmt(
        r#"
            fn f(n: Int32) -> String
              cond
                n > 2 -> "big"

                # before else
                else -> "small"
              end
            end
        "#,
        r#"
            fn f(n: Int32) -> String
              cond
                n > 2 ->
                  "big"

                # before else
                else ->
                  "small"
              end
            end
        "#,
    );
}

#[test]
fn adjacent_comment_before_cond_else_stays_with_previous_arm() {
    assert_fmt(
        r#"
            fn f(n: Int32) -> String
              cond
                n > 2 -> "big"
                # explains the previous case
                else -> "small"
              end
            end
        "#,
        r#"
            fn f(n: Int32) -> String
              cond
                n > 2 ->
                  "big"
                  # explains the previous case

                else ->
                  "small"
              end
            end
        "#,
    );
}

#[test]
fn comment_before_match_end_stays_inside() {
    assert_unchanged(
        r#"
            fn f -> String
              r =
                match 1
                  _ -> "x"
                  # inside comment
                end

              r
            end
        "#,
    );
}

#[test]
fn receive_after_boundary_comments_stay_in_place() {
    assert_fmt(
        r#"
            fn f
              receive
                msg: Int -> msg.print()

              # before after
              after 10 # timeout ms
                "x".print()
              end
            end
        "#,
        r#"
            fn f
              receive
                msg: Int ->
                  msg.print()

              # before after
              after 10 # timeout ms
                "x".print()
              end
            end
        "#,
    );
}

#[test]
fn adjacent_comment_before_receive_after_stays_with_previous_arm() {
    assert_fmt(
        r#"
            fn f
              receive
                msg: Int -> msg.print()
              # explains message handling
              after 10
                "x".print()
              end
            end
        "#,
        r#"
            fn f
              receive
                msg: Int ->
                  msg.print()
                  # explains message handling
              after 10
                "x".print()
              end
            end
        "#,
    );
}
