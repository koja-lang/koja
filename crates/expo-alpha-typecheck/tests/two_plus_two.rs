//! End-to-end smoke test for the alpha typecheck pipeline at its POC scope.
//!
//! Drives `parse_program → check_program` on `fn main; 2 + 2; end` and
//! asserts:
//!
//! 1. The pipeline succeeds (no typecheck diagnostics).
//! 2. The registry contains `TestApp.main` registered as a function.
//! 3. The body's `2 + 2` expression carries a populated `resolved_type`
//!    of `Type::Primitive(I64)` — proof that resolve + seal both ran.
//!
//! When this test passes the alpha typecheck phase has end-to-end coverage
//! sufficient for the next slice (lowering + eval).

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckedProgram, check_program};
use expo_ast::ast::{Expr, ExprKind, Item, Statement};
use expo_ast::identifier::Identifier;
use expo_ast::types::{Primitive, Type};
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn typecheck(source: &str) -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("two_plus_two.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    check_program(parsed).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed on `{source}`: {} diagnostic(s):\n{failure}",
            failure.diagnostics.len()
        )
    })
}

fn main_body(checked: &CheckedProgram) -> &[Statement] {
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
    main.body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point")
}

#[test]
fn fn_main_two_plus_two_typechecks_to_int() {
    let checked = typecheck("fn main\n  2 + 2\nend\n");

    let main_id = Identifier::new(PACKAGE, vec!["main".to_string()]);
    assert!(
        checked.registry.lookup(&main_id).is_some(),
        "registry is missing `{main_id}`; registry: {:?}",
        checked.registry,
    );

    let body = main_body(&checked);
    assert_eq!(body.len(), 1, "expected exactly one statement in main");
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr at body[0], got {:?}", body[0]);
    };

    assert_eq!(
        expr.resolved_type.as_ref(),
        Some(&Type::Primitive(Primitive::I64)),
        "top-level `2 + 2` did not resolve to Int",
    );

    let ExprKind::Binary { left, right, .. } = &expr.kind else {
        panic!("expected ExprKind::Binary, got {:?}", expr.kind);
    };
    assert_int(left);
    assert_int(right);
}

#[test]
fn duplicate_fn_in_same_file_emits_diagnostic() {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("dup.expo"),
            source: "fn main\n  1\nend\n\nfn main\n  2\nend\n".to_string(),
        }],
        ParseMode::File,
    );
    let failure = check_program(parsed).expect_err("duplicate fn should fail typecheck");
    assert_eq!(
        failure.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got {failure}",
    );
    let diag = &failure.diagnostics[0];
    assert!(
        diag.message.contains("`TestApp.main`") && diag.message.contains("already defined"),
        "unexpected diagnostic message: {}",
        diag.message,
    );
}

fn assert_int(expr: &Expr) {
    assert_eq!(
        expr.resolved_type.as_ref(),
        Some(&Type::Primitive(Primitive::I64)),
        "operand did not resolve to Int: {expr:?}",
    );
}
