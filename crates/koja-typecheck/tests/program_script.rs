//! Coverage for script-mode (`File.body` populated) typechecking.
//!
//! `resolve` walks `File.body` directly and `seal` accepts a populated
//! `File.body` as the script-mode shape. These tests pin that:
//! `resolve` populates `Statement::Expr.resolution`, `seal_ast`
//! accepts the resulting program (a panic would fail the test), and
//! script files coexist with `File.items[Function]` decls.

use koja_ast::ast::{ExprKind, Item, Statement};
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_parser::ParseMode;

mod common;

use common::{PACKAGE, global_id, registry_id, script_body, test_file, typecheck};

#[test]
fn script_body_resolves_top_level_expression() {
    let checked = typecheck("2 + 2\n", ParseMode::Script);

    let int_id = global_id(&checked, "Int");
    let body = script_body(&checked);
    assert_eq!(body.len(), 1, "expected one statement on File.body");

    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr at body[0], got {:?}", body[0]);
    };
    assert!(
        expr.resolution.is_resolved(),
        "top-level `2 + 2` carried unresolved type after resolve",
    );
    assert_eq!(
        expr.resolution,
        ResolvedType::leaf(Resolution::Global(int_id)),
        "top-level `2 + 2` did not resolve to Global.Int",
    );
}

#[test]
fn script_body_with_helper_fn_resolves_call_through_packages() {
    let source = "fn helper -> Int\n  1\nend\n\nhelper() + 1\n";

    let checked = typecheck(source, ParseMode::Script);
    let int_id = global_id(&checked, "Int");

    let file = test_file(&checked);
    assert_eq!(file.items.len(), 1, "expected helper fn item");
    let Item::Function(helper) = &file.items[0] else {
        panic!("expected Item::Function, got {:?}", file.items[0]);
    };
    assert_eq!(helper.name, "helper");

    let body = script_body(&checked);
    assert_eq!(body.len(), 1);
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr, got {:?}", body[0]);
    };
    assert_eq!(
        expr.resolution,
        ResolvedType::leaf(Resolution::Global(int_id)),
        "`helper() + 1` did not resolve to Global.Int",
    );

    let ExprKind::Binary { left, .. } = &expr.kind else {
        panic!("expected Binary, got {:?}", expr.kind);
    };
    let ExprKind::Call { callee, .. } = &left.kind else {
        panic!("expected Call as left operand, got {:?}", left.kind);
    };
    let ExprKind::Ident { resolution, .. } = &callee.kind else {
        panic!("expected bare-Ident callee, got {:?}", callee.kind);
    };
    let helper_id = registry_id(&checked, PACKAGE, &["helper"]);
    assert_eq!(*resolution, Resolution::Global(helper_id));
}

#[test]
fn project_mode_file_keeps_body_none() {
    let source = "fn main\n  2 + 2\nend\n";
    let checked = typecheck(source, ParseMode::File);

    let file = test_file(&checked);
    assert!(
        file.body.is_none(),
        "project-mode files must leave File.body as None; got {:?}",
        file.body,
    );
    assert_eq!(file.items.len(), 1);
    assert!(matches!(file.items[0], Item::Function(_)));
}

#[test]
fn script_seal_accepts_body_populated_file() {
    // `check_program` runs `seal_ast` on the success branch. Reaching
    // `Ok(_)` here means the post-flip seal accepted a populated
    // `File.body`. A regression to the pre-flip behaviour would
    // surface as a panic during `check_program`.
    let _ = typecheck("2 + 2\n", ParseMode::Script);
}
