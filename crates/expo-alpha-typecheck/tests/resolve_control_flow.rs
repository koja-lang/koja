//! Typecheck coverage for `if` (no-`else`) and `unless` statements.
//!
//! These tests pin the contract: condition must be `Bool`, body
//! statements resolve under the same rules as anywhere else, and
//! the whole expression resolves to `Unit` because value-producing
//! `if`/`else` is deferred to the locals slice. `else` is rejected
//! as a feature gap.

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

fn unit_type(checked: &CheckedProgram) -> ResolvedType {
    let ident = Identifier::new("Global", vec!["Unit".to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .expect("stdlib stub `Global.Unit` missing from registry");
    ResolvedType::leaf(Resolution::Global(id))
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
fn if_with_else_branch_diagnoses_feature_gap() {
    let source = "
        fn main
          if true
            1
          else
            2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not yet support `else`")),
        "expected `else`-branch diagnostic, got: {:?}",
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
