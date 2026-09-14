mod common;

use common::*;

#[test]
fn or_pattern_short_stays_inline() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 | 2 | 3 -> 0
                _ -> x
              end
            end
        ",
    );
}

#[test]
fn or_pattern_long_wraps_with_trailing_pipe() {
    assert_fmt(
        r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                _ -> false
              end
            end
        "#,
        r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
    );
}

#[test]
fn or_pattern_multiline_arms_have_blank_lines() {
    assert_fmt(
        r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" | "y" | "z" -> true
                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" | "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" | "Y" | "Z" -> true
                _ -> false
              end
            end
        "#,
        r#"
            fn f(s: String) -> Bool
              match s
                "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j" | "k" | "l" |
                "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t" | "u" | "v" | "w" | "x" |
                "y" | "z" ->
                  true

                "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J" | "K" | "L" |
                "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T" | "U" | "V" | "W" | "X" |
                "Y" | "Z" ->
                  true

                _ ->
                  false
              end
            end
        "#,
    );
}

#[test]
fn cond_chain_with_not() {
    assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999 or not x == y -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and not x == 50 and y > 0 and not y == 50 or x == 999 or y == 999
                  or not x == y ->
                  true

                else ->
                  false
              end
            end
        ");
}

#[test]
fn cond_and_chain_packs_like_fill() {
    assert_fmt("
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and y != 50 and x != 99 -> true
                else -> false
              end
            end
        ", "
            fn f(x: Int, y: Int) -> Bool
              cond
                x > 0 and x < 100 and y > 0 and y < 100 and x != y and x != 50 and y != 50
                  and x != 99 ->
                  true

                else ->
                  false
              end
            end
        ");
}

#[test]
fn wrapped_cond_arm_head_is_stable() {
    assert_unchanged_script(
        "
            v =
              cond
                no ->
                  \"first\"

                yes and yes and yes and yes and yes and yes and yes and yes and yes and yes
                  and yes and yes ->
                  \"wrapped\"

                else ->
                  \"other\"
              end
        ",
    );
}

#[test]
fn wrapped_match_guard_is_stable() {
    assert_unchanged_script(
        "
            r =
              match n
                0 -> \"zero\"
                x when x > 0 and x < 100 and x != 13 and x != 42 and x != 99 and x != 7
                  and x != 3 ->
                  \"ok\"
                _ -> \"other\"
              end
        ",
    );
}

#[test]
fn cond_or_chain_packs_like_fill() {
    assert_fmt(
        r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta" or x == "echo" or x == "foxtrot" or x == "golf" -> "nato"
                else -> "other"
              end
            end
        "#,
        r#"
            fn f(x: String) -> String
              cond
                x == "alpha" or x == "bravo" or x == "charlie" or x == "delta"
                  or x == "echo" or x == "foxtrot" or x == "golf" ->
                  "nato"

                else ->
                  "other"
              end
            end
        "#,
    );
}

#[test]
fn match_long_single_expr_body_breaks_all_arms() {
    // One arm's single-expression body overflows the page width, so
    // every sibling arm body (not just the long one) is pushed onto
    // its own line with blank lines between arms.
    assert_fmt(
        r#"
            fn pick(x: Option<Int>) -> String
              match x
                Option.Some(value) -> compute_the_chosen_label_for_the_final_display(value)
                Option.None -> "none"
              end
            end
        "#,
        r#"
            fn pick(x: Option<Int>) -> String
              match x
                Option.Some(value) ->
                  compute_the_chosen_label_for_the_final_display(value)

                Option.None ->
                  "none"
              end
            end
        "#,
    );
}

#[test]
fn match_short_bodies_stay_inline() {
    // Regression guard: when no arm body overflows, the whole match
    // stays inline with no blank lines between arms.
    assert_fmt(
        "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                _ -> 0
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              match x
                1 -> 10
                _ -> 0
              end
            end
        ",
    );
}

#[test]
fn match_ternary_body_breaks_arms_consistently() {
    // The `scan_error` shape: a ternary body overflows, so the short
    // sibling arm breaks consistently rather than staying inline.
    assert_fmt(
        r#"
            fn scan_error(field: Int, rest: Binary) -> String
              match take_cstring(rest)
                Option.Some(pair) -> field == 77 ? pair.first : scan_error_more(pair.second)
                Option.None -> "error"
              end
            end
        "#,
        r#"
            fn scan_error(field: Int, rest: Binary) -> String
              match take_cstring(rest)
                Option.Some(pair) ->
                  field == 77 ? pair.first : scan_error_more(pair.second)

                Option.None ->
                  "error"
              end
            end
        "#,
    );
}
