mod common;

use common::*;

#[test]
fn short_closure_inline() {
    assert_fmt_script(
        "
            fn apply(f: fn(Int) -> Int, x: Int) -> Int
              f(x)
            end

            apply(x -> x * 2, 5)
        ",
        "
            fn apply(f: fn (Int) -> Int, x: Int) -> Int
              f(x)
            end

            apply(x -> x * 2, 5)
        ",
    );
}

#[test]
fn short_block_closure_assignment_stays_inline() {
    assert_fmt_script(
        "
            f =
              fn (x: Int, y: Int) -> Int x + y end
        ",
        "
            f = fn (x: Int, y: Int) -> Int x + y end
        ",
    );
}

#[test]
fn long_block_closure_assignment_breaks_after_eq() {
    assert_fmt_script(
        "
            transform = fn (input_value: Int, scaling_factor: Int) -> Int input_value * scaling_factor + 1 end
        ",
        "
            transform =
              fn (input_value: Int, scaling_factor: Int) -> Int
                input_value * scaling_factor + 1
              end
        ",
    );
}

#[test]
fn closure_body_trailing_comment_forces_broken_layout() {
    assert_fmt_script(
        "
            add = fn (a: Int32) -> Int32
              a # body comment
            end
        ",
        "
            add =
              fn (a: Int32) -> Int32
                a # body comment
              end
        ",
    );
    assert_unchanged_script(
        "
            add =
              fn (a: Int32) -> Int32
                a # body comment
              end
        ",
    );
}

#[test]
fn closure_body_leading_comment_forces_broken_layout() {
    assert_unchanged_script(
        "
            g =
              fn (x: Int32) -> Int32
                # double it
                x * 2
              end
        ",
    );
}

#[test]
fn closure_end_line_comment_stays_inline() {
    assert_unchanged_script("f = fn () -> Int 42 end # note");
}

#[test]
fn sole_short_closure_arg_does_not_explode() {
    // An overflowing single call with a sole short-closure argument stays
    // hugged on one line rather than exploding the arg list.
    assert_fmt(
        r#"
            fn f(opt: Option<Int>) -> Option<Int>
              some_extremely_long_receiver_variable_name_here.map(value -> value_plus_something)
            end
        "#,
        r#"
            fn f(opt: Option<Int>) -> Option<Int>
              some_extremely_long_receiver_variable_name_here.map(value -> value_plus_something)
            end
        "#,
    );
}

#[test]
fn sole_block_closure_arg_hugs_and_breaks_internally() {
    // A sole block-closure argument hugs the parens and breaks inside the
    // closure body, with `end)` closing both the closure and the call.
    assert_fmt(
        r#"
            fn f(nums: List<Int>) -> List<Int>
              nums.map(fn (n: Int) -> Int compute_a_doubled_display_value_for_each_number(n) end)
            end
        "#,
        r#"
            fn f(nums: List<Int>) -> List<Int>
              nums.map(fn (n: Int) -> Int
                compute_a_doubled_display_value_for_each_number(n)
              end)
            end
        "#,
    );
}

#[test]
fn short_call_with_inline_closure_stays_glued() {
    // The assignment only breaks when the value renders as a
    // multi-line block. An inline-fitting closure does not count.
    assert_unchanged_script("ref = Task.async(fn () -> Int 42 end)");
}

#[test]
fn call_with_multiline_closure_breaks_after_equals() {
    assert_fmt_script(
        "
            ref = Task.async(fn () -> Int
              a = 1
              a + 1
            end)
        ",
        "
            ref =
              Task.async(fn () -> Int
                a = 1
                a + 1
              end)
        ",
    );
}

#[test]
fn closure_with_block_body_takes_broken_layout() {
    // A single-statement body that is itself a block never
    // collapses onto the signature line.
    assert_unchanged_script(
        r#"
            g =
              fn () -> String
                match 1
                  1 -> "a"
                  _ -> "b"
                end
              end
        "#,
    );
}
