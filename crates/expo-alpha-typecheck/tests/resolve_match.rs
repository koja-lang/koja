//! Typecheck coverage for `match`. Pin the contract:
//!
//! - subject + arm bodies resolve under the same rules as anywhere else
//! - the surface expression's type is the join of every reaching arm tail
//!   (with `Never` as the lattice bottom — divergent arms don't constrain it)
//! - a wildcard / binding catch-all is required (no enum-exhaustiveness yet)
//! - guard clauses, unsupported pattern shapes, and literal patterns over
//!   non-primitive subjects diagnose feature gaps
//! - bindings stamp a `LocalId` on the AST node

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{ExprKind, Item, Pattern, Statement};
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

fn int_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Int")
}

fn string_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "String")
}

#[test]
fn match_int_literal_arms_resolve_to_int() {
    let source = "
        fn main
          match 1
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_string_literal_arms_resolve_to_int() {
    let source = "
        fn main
          match \"hi\"
            \"hi\" -> 1
            _ -> 0
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_arm_resolves_to_subject_type() {
    let source = "
        fn main
          match 7
            x -> x
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_stamps_local_id() {
    let source = "
        fn main
          match 7
            x -> x
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
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("missing fn main");
    let body = main.body.as_deref().expect("missing fn main body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match-expr") else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Binding { local_id, name, .. } = &arms[0].pattern else {
        panic!("expected Pattern::Binding for arm 0");
    };
    assert_eq!(name, "x");
    assert!(
        local_id.is_some(),
        "binding `x` should carry a stamped LocalId"
    );
}

#[test]
fn match_with_diverging_arms_resolves_to_else_type() {
    let source = "
        fn pick -> Int
          match 1
            1 -> return 10
            _ -> 20
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
fn match_without_catch_all_diagnoses() {
    let source = "
        fn main
          match 1
            1 -> 10
            2 -> 20
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("must include a wildcard")),
        "expected catch-all diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_with_mismatched_arms_diagnoses() {
    let source = "
        fn main
          match 1
            1 -> 10
            _ -> false
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("match arms have inconsistent types")),
        "expected mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_with_literal_type_mismatch_diagnoses() {
    let source = "
        fn main
          match 1
            \"hi\" -> 10
            _ -> 20
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not match subject type")),
        "expected literal-vs-subject diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_guard_diagnoses_feature_gap() {
    let source = "
        fn main
          match 1
            x when x > 0 -> 10
            _ -> 20
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not yet support `when` guards")),
        "expected guard feature-gap diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_string_subject_resolves() {
    let source = "
        fn main
          match \"hi\"
            \"hi\" -> \"yes\"
            _ -> \"no\"
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), string_type(&checked));
}
