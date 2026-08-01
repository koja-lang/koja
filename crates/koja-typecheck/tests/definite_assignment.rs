//! Definite-assignment coverage. A read of a local is rejected
//! unless every path from function entry assigns it first: branch
//! arms join by intersection, loop bodies are maybe-executed, and
//! diverging arms (`return`, `break`, `panic`) don't veto joins.
//! Writes stay legal on every path, so reassignment after a
//! zero-trip loop still compiles.

use koja_ast::util::dedent;

mod common;

use common::{assert_file_fails_with, assert_script_fails_with, typecheck_file, typecheck_script};

const NEEDLE: &str = "may not be assigned on every path";

// Rejections

#[test]
fn read_after_zero_trip_while_diagnoses() {
    assert_script_fails_with(
        "
        i = 5
        while i < 3
          n = i * 2
        end
        n.print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn read_after_if_without_else_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          n = 1
        end
        n.print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn read_after_partially_assigning_if_else_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          n = 1
        else
          flag = false
        end
        n.print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn read_after_partially_assigning_cond_diagnoses() {
    assert_script_fails_with(
        "
        x = 5
        cond
          x > 3 -> n = 1
          else -> ()
        end
        n.print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn compound_assign_on_maybe_assigned_local_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          count = 0
        end
        count += 1
        ",
        &[NEEDLE],
    );
}

#[test]
fn field_write_on_maybe_assigned_local_diagnoses() {
    assert_file_fails_with(
        "
        struct Point
          x: Int
          y: Int
        end

        fn build(flag: Bool) -> Int
          if flag
            p = Point{x: 1, y: 2}
          end
          p.x = 3
          0
        end
        ",
        &[NEEDLE],
    );
}

#[test]
fn closure_capturing_maybe_assigned_local_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          greeting = \"hi\"
        end
        f = fn () -> String
          greeting
        end
        f().print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn read_inside_later_branch_of_maybe_assigned_local_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          n = 1
        end
        if flag
          n.print()
        end
        ",
        &[NEEDLE],
    );
}

#[test]
fn loop_body_assignment_does_not_reach_past_the_loop() {
    // Conservative: nothing a `loop` body assigns survives the loop,
    // even ahead of an unconditional `break`.
    assert_script_fails_with(
        "
        loop
          n = 1
          break
        end
        n.print()
        ",
        &[NEEDLE],
    );
}

#[test]
fn interpolation_read_of_maybe_assigned_local_diagnoses() {
    assert_script_fails_with(
        "
        flag = true
        if flag
          name = \"koja\"
        end
        \"hello #{name}\".print()
        ",
        &[NEEDLE],
    );
}

// Legal shapes

#[test]
fn straight_line_assign_then_read_compiles() {
    typecheck_script(&dedent(
        "
        x = 10
        x.print()
        ",
    ));
}

#[test]
fn both_if_arms_assigning_compiles() {
    typecheck_script(&dedent(
        "
        flag = true
        if flag
          n = 1
        else
          n = 2
        end
        n.print()
        ",
    ));
}

#[test]
fn every_cond_arm_assigning_compiles() {
    typecheck_script(&dedent(
        "
        x = 5
        cond
          x > 3 -> n = 1
          else -> n = 2
        end
        n.print()
        ",
    ));
}

#[test]
fn every_match_arm_reassigning_compiles() {
    typecheck_script(&dedent(
        "
        x = 1
        flag = x > 0
        if flag
          n = 10
        end
        match x
          1 -> n = 1
          _ -> n = 2
        end
        n.print()
        ",
    ));
}

#[test]
fn diverging_else_arm_compiles() {
    typecheck_file(&dedent(
        "
        fn pick(flag: Bool) -> Int
          if flag
            n = 1
          else
            return 0
          end
          n
        end
        ",
    ));
}

#[test]
fn panicking_else_arm_compiles() {
    typecheck_file(&dedent(
        "
        fn pick(flag: Bool) -> Int
          if flag
            n = 1
          else
            Kernel.panic(\"no value\")
          end
          n
        end
        ",
    ));
}

#[test]
fn reads_inside_loop_body_of_body_assigned_local_compile() {
    typecheck_script(&dedent(
        "
        i = 0
        while i < 10
          doubled = i * 2
          doubled.print()
          i += 1
        end
        ",
    ));
}

#[test]
fn write_after_zero_trip_loop_compiles() {
    // Only reads are gated. Reassignment of a loop-declared slot
    // after the loop is the existing `local_reuse_across_scopes`
    // pattern and must stay legal.
    typecheck_script(&dedent(
        "
        j = 5
        while j < 3
          s = \"loop value\"
          j += 1
        end
        s = \"post-loop value\"
        s.print()
        ",
    ));
}

#[test]
fn match_pattern_bindings_are_assigned_in_guard_and_body() {
    typecheck_script(&dedent(
        "
        x = Option.Some(4)
        match x
          Option.Some(v) when v > 3 -> v.print()
          Option.Some(v) -> v.print()
          Option.None -> ()
        end
        ",
    ));
}

#[test]
fn closure_params_and_definite_captures_compile() {
    typecheck_script(&dedent(
        "
        base = 10
        add = fn (x: Int) -> Int
          base + x
        end
        add(1).print()
        ",
    ));
}

#[test]
fn value_producing_if_sidesteps_the_analysis() {
    typecheck_script(&dedent(
        "
        flag = true
        size = if flag
          \"big\"
        else
          \"small\"
        end
        size.print()
        ",
    ));
}
