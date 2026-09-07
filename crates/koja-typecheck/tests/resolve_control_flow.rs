//! Typecheck coverage for `if` / `else`, `unless`, and `cond`.
//!
//! These tests pin the contract: condition must be `Bool`, body
//! statements resolve under the same rules as anywhere else, and
//! the surface expression resolves to the join of every reaching
//! arm tail (with `Never` as the lattice bottom: divergent arms
//! contribute `Never` and don't constrain the join).
//!
//! `if` without `else` keeps statement-shape `Unit` typing.
//! `unless` is statement-only (no else arm), so it's always `Unit`.
//! Mismatched arm types surface a diagnostic and the surface
//! expression resolves to `Unresolved`.

use koja_ast::identifier::ResolvedType;
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    assert_script_fails_with, bool_type, function_body, global_named, int_type, last_expr,
    never_type, string_type, trailing_resolution, typecheck_script as typecheck, unit_type,
};

#[test]
fn if_with_bool_condition_resolves_to_unit() {
    let source = "
        if true
          1
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn unless_with_bool_condition_resolves_to_unit() {
    let source = "
        unless false
          1
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn if_with_int_condition_diagnoses() {
    let source = "
        if 1
          2
        end
        ";
    assert_script_fails_with(source, &["`if` condition must be `Bool`"]);
}

#[test]
fn unless_with_int_condition_diagnoses() {
    let source = "
        unless 1
          2
        end
        ";
    assert_script_fails_with(source, &["`unless` condition must be `Bool`"]);
}

#[test]
fn if_else_with_matching_int_arms_resolves_to_int() {
    let source = "
        if true
          1
        else
          2
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn if_else_with_mismatched_arms_diagnoses() {
    let source = "
        if true
          1
        else
          true
        end
        ";
    assert_script_fails_with(source, &["if/else arms have inconsistent types"]);
}

#[test]
fn if_else_with_diverging_then_arm_resolves_to_else_type() {
    let source = "
        fn pick -> Int
          if true
            return 1
          else
            2
          end
        end

        pick()
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn if_else_with_both_arms_diverging_resolves_to_never() {
    let source = "
        fn diverge -> Int
          if true
            return 1
          else
            return 2
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let if_expr = last_expr(function_body(&checked, "diverge"));
    assert_eq!(if_expr.resolution, never_type(&checked));
}

#[test]
fn cond_with_matching_int_arms_resolves_to_int() {
    let source = "
        cond
          true -> 1
          false -> 2
          else -> 3
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn cond_with_mismatched_arms_diagnoses() {
    let source = "
        cond
          true -> 1
          false -> false
          else -> 3
        end
        ";
    assert_script_fails_with(source, &["cond arms have inconsistent types"]);
}

#[test]
fn cond_with_int_condition_diagnoses() {
    let source = "
        cond
          1 -> 1
          else -> 2
        end
        ";
    assert_script_fails_with(source, &["`cond` condition must be `Bool`"]);
}

#[test]
fn cond_with_diverging_arms_joins_against_else() {
    let source = "
        fn pick -> Int
          cond
            true -> return 1
            false -> return 2
            else -> 3
          end
        end

        pick()
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn ternary_with_matching_int_arms_resolves_to_int() {
    let source = "
        true ? 1 : 2
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn ternary_with_mismatched_arms_diagnoses() {
    let source = "
        true ? 1 : false
        ";
    assert_script_fails_with(source, &["ternary arms have inconsistent types"]);
}

#[test]
fn ternary_with_int_condition_diagnoses() {
    let source = "
        1 ? 2 : 3
        ";
    assert_script_fails_with(source, &["`ternary` condition must be `Bool`"]);
}

#[test]
fn nested_if_inside_unless_resolves_to_unit() {
    let source = "
        unless false
          if true
            1
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

/// `Result<Bool, String>` as the harness's `ResolvedType`.
fn result_bool_string(checked: &CheckedProgram) -> ResolvedType {
    global_named(
        checked,
        "Result",
        vec![bool_type(checked), string_type(checked)],
    )
}

#[test]
fn if_else_binding_joins_result_payloads_across_arms() {
    let source = "
        flag = true
        r = if flag
          Result.Ok(true)
        else
          Result.Err(\"nope\")
        end
        r
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), result_bool_string(&checked));
}

#[test]
fn cond_binding_joins_result_payloads_across_arms() {
    let source = "
        flag = true
        r = cond
          flag -> Result.Ok(true)
          not flag -> Result.Ok(false)
          else -> Result.Err(\"nope\")
        end
        r
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), result_bool_string(&checked));
}

#[test]
fn ternary_binding_fills_unit_variant_from_sibling_arm() {
    let source = "
        flag = true
        o = flag ? Option.Some(1) : Option.None
        o
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(
        trailing_resolution(&checked),
        global_named(&checked, "Option", vec![int_type(&checked)])
    );
}

#[test]
fn cond_binding_with_holes_in_every_arm_still_cannot_infer() {
    let source = "
        flag = true
        o = cond
          flag -> Option.None
          else -> Option.None
        end
        ";
    assert_script_fails_with(
        source,
        &["cannot infer type parameter `T` of `Global.Option`"],
    );
}
