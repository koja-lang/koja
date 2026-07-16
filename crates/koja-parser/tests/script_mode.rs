//! Behavioral tests for `ParseMode::Script`.
//!
//! Covers the disambiguation rules between top-level items and
//! top-level statements that script mode introduces, plus the
//! regression-safety guarantee that file-mode behavior is unchanged.
//!
//! Higher-level coverage of script-mode typechecking lives in
//! `koja-typecheck/tests/script_mode.rs`.

use koja_ast::ast::{ExprKind, Item, Literal, Statement};

mod common;

use common::{parse_clean, parse_clean_script, parse_failing};

#[test]
fn file_mode_rejects_top_level_expression() {
    let result = parse_failing("2 + 2\n");
    assert!(
        result.ast.body.is_none(),
        "file mode never populates File.body"
    );
}

#[test]
fn script_mode_accepts_bare_expression() {
    let file = parse_clean_script("2 + 2\n");
    assert!(
        file.items.is_empty(),
        "no items expected, got {:?}",
        file.items,
    );
    let body = file
        .body
        .as_ref()
        .expect("script mode populates File.body when statements are present");
    assert_eq!(body.len(), 1);
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr at body[0], got {:?}", body[0]);
    };
    assert!(matches!(expr.kind, ExprKind::Binary { .. }));
}

#[test]
fn script_mode_accepts_mixed_items_and_statements() {
    let file = parse_clean_script("fn helper\n  1\nend\n\n2 + helper()\n");

    assert_eq!(file.items.len(), 1, "expected exactly one item");
    let Item::Function(function) = &file.items[0] else {
        panic!("expected Item::Function, got {:?}", file.items[0]);
    };
    assert_eq!(function.name, "helper");

    let body = file
        .body
        .as_ref()
        .expect("body must be Some when there are top-level statements");
    assert_eq!(body.len(), 1);
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr, got {:?}", body[0]);
    };
    assert!(matches!(expr.kind, ExprKind::Binary { .. }));
}

#[test]
fn script_mode_disambiguates_fn_item_from_closure_expr() {
    // `fn(...) -> ... end` is a block-form closure expression. The
    // disambiguator must treat `Fn` followed by `LParen` as an
    // expression starter, not as `fn name(...)` (which requires
    // `Fn` followed by an identifier).
    let file = parse_clean_script("fn() -> Int\n  42\nend\nfn helper\n  2\nend\n");

    assert_eq!(file.items.len(), 1, "expected exactly one fn item");
    let Item::Function(function) = &file.items[0] else {
        panic!("expected Item::Function, got {:?}", file.items[0]);
    };
    assert_eq!(function.name, "helper");

    let body = file
        .body
        .as_ref()
        .expect("closure expression should be lifted into File.body");
    assert_eq!(body.len(), 1);
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr, got {:?}", body[0]);
    };
    assert!(
        matches!(expr.kind, ExprKind::Closure { .. }),
        "expected Closure ExprKind, got {:?}",
        expr.kind,
    );
}

#[test]
fn script_mode_with_only_items_leaves_body_none() {
    let file = parse_clean_script("fn main\n  2 + 2\nend\n");
    assert_eq!(file.items.len(), 1);
    assert!(
        file.body.is_none(),
        "items-only script must collapse File.body to None so downstream passes can distinguish it from statement-bearing scripts",
    );
}

#[test]
fn script_mode_handles_assignment_at_top_level() {
    let file = parse_clean_script("x = 5\nx + 1\n");
    let body = file.body.as_ref().expect("expected populated body");
    assert_eq!(body.len(), 2);
    assert!(matches!(body[0], Statement::Assignment { .. }));
    assert!(matches!(body[1], Statement::Expr(_)));
}

#[test]
fn file_mode_after_script_mode_changes_unaffected() {
    let file = parse_clean("fn main\n  42\nend\n");
    assert!(file.body.is_none());
    assert_eq!(file.items.len(), 1);
    let Item::Function(function) = &file.items[0] else {
        panic!("expected Item::Function");
    };
    assert_eq!(function.name, "main");
    let body = function.body.as_ref().expect("fn main has a body");
    assert_eq!(body.len(), 1);
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr inside fn main");
    };
    assert!(matches!(
        &expr.kind,
        ExprKind::Literal { value: Literal::Int(s) } if s == "42"
    ));
}
