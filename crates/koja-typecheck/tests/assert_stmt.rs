//! Typecheck pins for the `assert` statement.
//!
//! - **Channel rule**: `assert` type checks only when the enclosing
//!   function declares `! Test.Failure`. A missing channel, a
//!   different channel, and a bundle without the `Test` package each
//!   get their own teacher diagnostic.
//! - **Desugar**: a comparison form binds both operands to locals once
//!   and renders them through `.format()`. Any other condition keeps
//!   only its source text with `Option.None` operands. The message is
//!   built inside the `if`, so it is evaluated only on failure.
//! - **Position**: an `assert` embedded in a larger expression is an
//!   error, like `fail`.

use std::path::PathBuf;

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, FieldInit, Statement, name_texts};
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::check_program;

mod common;

use common::{PACKAGE, function_body, typecheck_file, typecheck_file_fail};

fn assert_fails_with_hint(source: &str, message_needle: &str, hint_needle: &str) {
    let failure = typecheck_file_fail(&dedent(source));
    let found = failure.diagnostics.iter().any(|d| {
        d.message.contains(message_needle)
            && d.hint.as_deref().is_some_and(|h| h.contains(hint_needle))
    });
    assert!(
        found,
        "expected a diagnostic with message containing `{message_needle}` and hint \
         containing `{hint_needle}`, got: {:#?}",
        failure.diagnostics,
    );
}

/// The `if not (...) fail ... end` statement the desugar emits, with
/// the `Test.Assertion` fields it constructs.
fn assertion_fields(statement: &Statement) -> &[FieldInit] {
    let Statement::Expr(Expr {
        kind: ExprKind::If { then_body, .. },
        ..
    }) = statement
    else {
        panic!("expected the desugared `if`, got {statement:?}");
    };
    let Some(Statement::Return {
        value: Some(returned),
        ..
    }) = then_body.first()
    else {
        panic!("expected `fail` to desugar to a return, got {then_body:?}");
    };
    let ExprKind::EnumConstruction {
        data: EnumConstructionData::Tuple(err_args),
        ..
    } = &returned.kind
    else {
        panic!("expected Result.Err(...), got {returned:?}");
    };
    let ExprKind::EnumConstruction {
        type_path,
        variant,
        data: EnumConstructionData::Tuple(failure_args),
    } = &err_args[0].kind
    else {
        panic!(
            "expected Test.Failure.Assertion(...), got {:?}",
            err_args[0]
        );
    };
    assert_eq!(type_path, &["Test", "Failure"]);
    assert_eq!(variant, "Assertion");
    let ExprKind::StructConstruction { type_path, fields } = &failure_args[0].kind else {
        panic!("expected Test.Assertion{{...}}, got {:?}", failure_args[0]);
    };
    assert_eq!(type_path, &["Test", "Assertion"]);
    fields
}

fn field<'a>(fields: &'a [FieldInit], name: &str) -> &'a Expr {
    &fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| panic!("no `{name}` field"))
        .value
}

fn option_variant(expr: &Expr) -> &str {
    let ExprKind::EnumConstruction {
        type_path, variant, ..
    } = &expr.kind
    else {
        panic!("expected an Option construction, got {expr:?}");
    };
    assert_eq!(name_texts(type_path), ["Option"]);
    variant.as_str()
}

fn string_literal(expr: &Expr) -> String {
    let ExprKind::String { parts, .. } = &expr.kind else {
        panic!("expected a string literal, got {expr:?}");
    };
    parts
        .iter()
        .map(|part| match part {
            koja_ast::ast::StringPart::Literal { value, .. } => value.clone(),
            other => panic!("expected literal text, got {other:?}"),
        })
        .collect()
}

#[test]
fn assert_needs_an_error_channel() {
    assert_fails_with_hint(
        "
        fn check(x: Int)
          assert x == 1
        end
        ",
        "`assert` needs `Test.Failure` on the error channel",
        "helper that declares `! Test.Failure`",
    );
}

#[test]
fn assert_rejects_a_different_channel() {
    assert_fails_with_hint(
        "
        fn check(x: Int) ! String
          assert x == 1
        end
        ",
        "`assert` needs `Test.Failure` on the error channel",
        "helper that declares `! Test.Failure`",
    );
}

#[test]
fn assert_without_the_test_package_names_the_package() {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources_for(false));
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.koja"),
        source: dedent(
            "
            fn check(x: Int) ! String
              assert x == 1
            end
            ",
        ),
    });
    let failure = check_program(parse_program(sources, ParseMode::File))
        .expect_err("assert without Test must fail");
    assert!(
        failure.diagnostics.iter().any(|d| {
            d.message.contains("`assert` needs the `Test` package")
                && d.hint
                    .as_deref()
                    .is_some_and(|h| h.contains("Kernel.panic"))
        }),
        "got: {:#?}",
        failure.diagnostics,
    );
}

#[test]
fn assert_cannot_be_embedded_in_an_expression() {
    assert_fails_with_hint(
        "
        fn check(x: Int) ! Test.Failure
          y = assert x == 1
        end
        ",
        "cannot be embedded in a larger expression",
        "as a statement of its own",
    );
}

#[test]
fn comparison_form_binds_operands_and_renders_both() {
    let checked = typecheck_file(&dedent(
        "
        fn check(x: Int) ! Test.Failure
          assert x + 1 == 2
        end
        ",
    ));
    let body = function_body(&checked, "check");
    let Statement::Assignment { target, .. } = &body[0] else {
        panic!("expected the left operand binding, got {:?}", body[0]);
    };
    assert_eq!(target.segments, ["$assert_left_0"]);
    let Statement::Assignment { target, .. } = &body[1] else {
        panic!("expected the right operand binding, got {:?}", body[1]);
    };
    assert_eq!(target.segments, ["$assert_right_0"]);

    let fields = assertion_fields(&body[2]);
    assert_eq!(string_literal(field(fields, "expression")), "x + 1 == 2");
    assert_eq!(
        string_literal(field(fields, "source_line")),
        "  assert x + 1 == 2"
    );
    assert_eq!(string_literal(field(fields, "file")), "test.koja");
    assert_eq!(option_variant(field(fields, "left")), "Some");
    assert_eq!(option_variant(field(fields, "right")), "Some");
    assert_eq!(option_variant(field(fields, "message")), "None");

    let ExprKind::EnumConstruction {
        data: EnumConstructionData::Tuple(rendered),
        ..
    } = &field(fields, "left").kind
    else {
        unreachable!("checked above");
    };
    assert!(
        matches!(&rendered[0].kind, ExprKind::MethodCall { method, .. } if method == "format"),
        "left operand renders through `.format()`, got {:?}",
        rendered[0]
    );
}

#[test]
fn other_conditions_keep_only_the_source_text() {
    let checked = typecheck_file(&dedent(
        "
        fn check(items: List<Int>) ! Test.Failure
          assert items.empty?(), \"left #{items.length()} behind\"
        end
        ",
    ));
    let body = function_body(&checked, "check");
    assert!(
        matches!(
            &body[0],
            Statement::Expr(Expr {
                kind: ExprKind::If { .. },
                ..
            })
        ),
        "no operand bindings for a non-comparison form, got {:?}",
        body[0]
    );
    let fields = assertion_fields(&body[0]);
    assert_eq!(
        string_literal(field(fields, "expression")),
        "items.empty?()"
    );
    assert_eq!(option_variant(field(fields, "left")), "None");
    assert_eq!(option_variant(field(fields, "right")), "None");
    // The message is built inside the `if`, so it only evaluates on
    // failure.
    assert_eq!(option_variant(field(fields, "message")), "Some");
}

#[test]
fn each_assert_gets_its_own_operand_slots() {
    let checked = typecheck_file(&dedent(
        "
        fn check(x: Int) ! Test.Failure
          assert x == 1
          assert x < 2
        end
        ",
    ));
    let body = function_body(&checked, "check");
    let names: Vec<&str> = body
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assignment { target, .. } => Some(target.segments[0].as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        [
            "$assert_left_0",
            "$assert_right_0",
            "$assert_left_1",
            "$assert_right_1"
        ]
    );
}
