//! Typecheck coverage for the boolean and comparison operators:
//! `and`, `or`, `not`, `== != < > <= >=`.
//!
//! Mirrors the `two_plus_two.rs` pattern: parse + check a small
//! `fn main` source, then inspect the resolved type of its trailing
//! expression. Error paths are covered by asserting a diagnostic on
//! ill-typed programs.

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program};
use expo_ast::ast::{Item, Statement};
use expo_ast::types::{Primitive, Type};
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

fn trailing_expr_type(checked: &CheckedProgram) -> Type {
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
        Statement::Expr(expr) => expr
            .resolved_type
            .clone()
            .expect("trailing expression has no resolved type"),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn bool_type() -> Type {
    Type::Primitive(Primitive::Bool)
}

fn int_type() -> Type {
    Type::Primitive(Primitive::I64)
}

#[test]
fn logical_and_or_resolve_to_bool() {
    assert_eq!(
        trailing_expr_type(&typecheck("fn main\n  true and false\nend\n")),
        bool_type(),
    );
    assert_eq!(
        trailing_expr_type(&typecheck("fn main\n  true or false\nend\n")),
        bool_type(),
    );
}

#[test]
fn unary_not_resolves_to_bool() {
    assert_eq!(
        trailing_expr_type(&typecheck("fn main\n  not true\nend\n")),
        bool_type(),
    );
}

#[test]
fn unary_neg_resolves_to_int() {
    assert_eq!(
        trailing_expr_type(&typecheck("fn main\n  -7\nend\n")),
        int_type(),
    );
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
        assert_eq!(
            trailing_expr_type(&typecheck(source)),
            bool_type(),
            "source = {source:?}",
        );
    }
}

#[test]
fn bool_equality_is_allowed() {
    assert_eq!(
        trailing_expr_type(&typecheck("fn main\n  true == false\nend\n")),
        bool_type(),
    );
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
