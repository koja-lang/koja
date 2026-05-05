//! Typecheck coverage for boolean and comparison operators
//! (`and`/`or`/`not`/`== != < > <= >=`). Mirrors `two_plus_two.rs`:
//! parse + check a tiny `fn main`, then inspect the trailing
//! expression's `resolution`. Error paths assert a diagnostic on
//! ill-typed programs.

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program};
use expo_ast::ast::{Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str) -> CheckedProgram {
    parse_and_check(source).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed on `{source}`: {} diagnostic(s):\n{failure}",
            failure.diagnostics.len()
        )
    })
}

fn typecheck_fail(source: &str) -> CheckFailure {
    parse_and_check(source).expect_err(
        "expected alpha typecheck to fail; it succeeded (test source must produce a diagnostic)",
    )
}

fn parse_and_check(source: &str) -> Result<CheckedProgram, CheckFailure> {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("boolean_ops.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    check_program(parsed)
}

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

/// Resolved leaf for the preloaded `Global.<name>` stdlib stub.
fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn bool_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Bool")
}

fn int_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Int")
}

fn assert_trailing_is(source: &str, expected_name: &str) {
    let checked = typecheck(source);
    let expected = global_leaf(&checked, expected_name);
    let actual = trailing_resolution(&checked);
    assert_eq!(
        actual, expected,
        "source = {source:?} did not resolve to Global.{expected_name}",
    );
}

#[test]
fn logical_and_or_resolve_to_bool() {
    assert_trailing_is("fn main\n  true and false\nend\n", "Bool");
    assert_trailing_is("fn main\n  true or false\nend\n", "Bool");
}

#[test]
fn unary_not_resolves_to_bool() {
    assert_trailing_is("fn main\n  not true\nend\n", "Bool");
}

#[test]
fn unary_neg_resolves_to_int() {
    assert_trailing_is("fn main\n  -7\nend\n", "Int");
}

#[test]
fn comparisons_resolve_to_bool() {
    for source in [
        "fn main\n  1 == 1\nend\n",
        "fn main\n  1 != 2\nend\n",
        "fn main\n  1 < 2\nend\n",
        "fn main\n  1 > 2\nend\n",
        "fn main\n  1 <= 2\nend\n",
        "fn main\n  1 >= 2\nend\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn bool_equality_is_allowed() {
    let checked = typecheck("fn main\n  true == false\nend\n");
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn int_type_helper_still_references_int() {
    // Sanity check that both `int_type` and `bool_type` correspond to
    // the stubs the resolver emits; catches reverse-index breakage.
    let checked = typecheck("fn main\n  1 + 1\nend\n");
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    assert_ne!(int_type(&checked), bool_type(&checked));
}

#[test]
fn mixed_int_and_bool_and_diagnoses() {
    let failure = typecheck_fail("fn main\n  1 and true\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("`and`"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn ordering_on_bool_diagnoses() {
    let failure = typecheck_fail("fn main\n  true < false\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int operands"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn not_on_int_diagnoses() {
    let failure = typecheck_fail("fn main\n  not 1\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Bool operand"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn neg_on_bool_diagnoses() {
    let failure = typecheck_fail("fn main\n  -true\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int operand"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}
