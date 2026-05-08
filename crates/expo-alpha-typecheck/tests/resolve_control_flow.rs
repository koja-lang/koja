//! Typecheck coverage for `if` / `else`, `unless`, and `cond`.
//!
//! These tests pin the contract: condition must be `Bool`, body
//! statements resolve under the same rules as anywhere else, and
//! the surface expression resolves to the join of every reaching
//! arm tail (with `Never` as the lattice bottom — divergent arms
//! contribute `Never` and don't constrain the join).
//!
//! `if` without `else` keeps statement-shape `Unit` typing.
//! `unless` is statement-only (no else arm), so it's always `Unit`.
//! Mismatched arm types surface a diagnostic and the surface
//! expression resolves to `Unresolved`.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{PACKAGE, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail};

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("file is missing `fn main`");
    let body = main
        .body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn primitive_type(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn unit_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Unit")
}

fn int_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Int")
}

fn never_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Never")
}

#[test]
fn if_with_bool_condition_resolves_to_unit() {
    let source = "
        fn main
          if true
            1
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn unless_with_bool_condition_resolves_to_unit() {
    let source = "
        fn main
          unless false
            1
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn if_with_int_condition_diagnoses() {
    let source = "
        fn main
          if 1
            2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`if` condition must be `Bool`")),
        "expected `if` condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn unless_with_int_condition_diagnoses() {
    let source = "
        fn main
          unless 1
            2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`unless` condition must be `Bool`")),
        "expected `unless` condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn if_else_with_matching_int_arms_resolves_to_int() {
    let source = "
        fn main
          if true
            1
          else
            2
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn if_else_with_mismatched_arms_diagnoses() {
    let source = "
        fn main
          if true
            1
          else
            true
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("if/else arms have inconsistent types")),
        "expected if/else mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
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

        fn main
          pick()
        end
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
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let diverge = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "diverge" => Some(f),
            _ => None,
        })
        .expect("missing `fn diverge`");
    let body = diverge.body.as_deref().expect("`fn diverge` has no body");
    let Statement::Expr(if_expr) = body.last().expect("missing trailing if-expr") else {
        panic!("expected trailing Statement::Expr");
    };
    assert_eq!(if_expr.resolution, never_type(&checked));
}

#[test]
fn cond_with_matching_int_arms_resolves_to_int() {
    let source = "
        fn main
          cond
            true -> 1
            false -> 2
            else -> 3
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn cond_with_mismatched_arms_diagnoses() {
    let source = "
        fn main
          cond
            true -> 1
            false -> false
            else -> 3
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cond arms have inconsistent types")),
        "expected cond mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn cond_with_int_condition_diagnoses() {
    let source = "
        fn main
          cond
            1 -> 1
            else -> 2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`cond` condition must be `Bool`")),
        "expected cond condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
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

        fn main
          pick()
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn ternary_with_matching_int_arms_resolves_to_int() {
    let source = "
        fn main
          true ? 1 : 2
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn ternary_with_mismatched_arms_diagnoses() {
    let source = "
        fn main
          true ? 1 : false
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("ternary arms have inconsistent types")),
        "expected ternary mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn ternary_with_int_condition_diagnoses() {
    let source = "
        fn main
          1 ? 2 : 3
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`ternary` condition must be `Bool`")),
        "expected ternary condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn nested_if_inside_unless_resolves_to_unit() {
    let source = "
        fn main
          unless false
            if true
              1
            end
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}
