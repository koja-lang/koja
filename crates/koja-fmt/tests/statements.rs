mod common;

use common::*;

#[test]
fn wrapped_condition_indents_and_separates_from_body() {
    // A condition too long for one line mirrors wrapped function
    // heads: continuation indented two past the keyword, blank
    // line before the body.
    assert_unchanged(
        "
            fn f(alpha: Bool, bravo: Bool, charlie: Bool, delta: Bool) -> Int
              if alpha and bravo and charlie and delta and alpha and bravo and charlie
                and delta

                1
              else
                2
              end
            end
        ",
    );
}

#[test]
fn wrapped_while_condition_indents_and_separates_from_body() {
    assert_unchanged(
        "
            fn f(first_operand: Bool, second_operand: Bool, third_operand: Bool) -> Int
              while first_operand and second_operand and third_operand and first_operand
                and second_operand

                1
              end

              2
            end
        ",
    );
}

#[test]
fn short_condition_stays_inline_without_blank_line() {
    assert_unchanged(
        "
            fn f(x: Int) -> Int
              if x > 10
                1
              else
                2
              end
            end
        ",
    );
}

#[test]
fn blank_line_before_block_statement() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              y = x + 1
              if y > 10
                y = y - 10
              end
              y
            end
        ",
        "
            fn f(x: Int) -> Int
              y = x + 1

              if y > 10
                y = y - 10
              end

              y
            end
        ",
    );
}

#[test]
fn no_blank_line_when_block_is_first_or_last() {
    assert_fmt(
        "
            fn f(x: Int) -> Int
              if x > 0
                x
              else
                -x
              end
            end
        ",
        "
            fn f(x: Int) -> Int
              if x > 0
                x
              else
                -x
              end
            end
        ",
    );
}

#[test]
fn blank_line_between_adjacent_blocks() {
    assert_fmt(
        "
            fn f(x: Int)
              if x > 0
                print(x)
              end
              while x > 0
                x -= 1
              end
            end
        ",
        "
            fn f(x: Int)
              if x > 0
                print(x)
              end

              while x > 0
                x -= 1
              end
            end
        ",
    );
}
