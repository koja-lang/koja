//! Behavioral tests for `ParseMode::Script`.
//!
//! Covers the disambiguation rules between top-level items and
//! top-level statements that script mode introduces, plus the
//! regression-safety guarantee that file-mode behavior is unchanged.
//!
//! Higher-level coverage of the lift pass lives in
//! `expo-alpha-typecheck/src/lift_script.rs`.

use expo_ast::ast::{ExprKind, Item, Literal, Statement};
use expo_parser::{ParseMode, parse};

#[test]
fn file_mode_rejects_top_level_expression() {
    let result = parse("2 + 2\n", ParseMode::File);
    assert!(
        !result.errors.is_empty(),
        "file mode must reject bare top-level expressions"
    );
    assert!(
        result.ast.body.is_none(),
        "file mode never populates File.body"
    );
}

#[test]
fn script_mode_accepts_bare_expression() {
    let result = parse("2 + 2\n", ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "script mode should accept `2 + 2`, got errors: {:?}",
        result.errors,
    );
    assert!(
        result.ast.items.is_empty(),
        "no items expected, got {:?}",
        result.ast.items,
    );
    let body = result
        .ast
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
    let source = "fn helper\n  1\nend\n\n2 + helper()\n";
    let result = parse(source, ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "mixed input should parse cleanly, got errors: {:?}",
        result.errors,
    );

    assert_eq!(result.ast.items.len(), 1, "expected exactly one item");
    let Item::Function(function) = &result.ast.items[0] else {
        panic!("expected Item::Function, got {:?}", result.ast.items[0]);
    };
    assert_eq!(function.name, "helper");

    let body = result
        .ast
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
    // `fn(...) -> ... end` is a block-form closure expression; the
    // disambiguator must treat `Fn` followed by `LParen` as an
    // expression starter, not as `fn name(...)` (which requires
    // `Fn` followed by an identifier).
    let source = "fn() -> Int\n  42\nend\nfn helper\n  2\nend\n";
    let result = parse(source, ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "fn closure expr + fn item should both parse, got errors: {:?}",
        result.errors,
    );

    assert_eq!(result.ast.items.len(), 1, "expected exactly one fn item");
    let Item::Function(function) = &result.ast.items[0] else {
        panic!("expected Item::Function, got {:?}", result.ast.items[0]);
    };
    assert_eq!(function.name, "helper");

    let body = result
        .ast
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
    let source = "fn main\n  2 + 2\nend\n";
    let result = parse(source, ParseMode::Script);
    assert!(result.errors.is_empty());
    assert_eq!(result.ast.items.len(), 1);
    assert!(
        result.ast.body.is_none(),
        "items-only script must collapse File.body to None so lift_script no-ops",
    );
}

#[test]
fn script_mode_handles_assignment_at_top_level() {
    let source = "x = 5\nx + 1\n";
    let result = parse(source, ParseMode::Script);
    assert!(
        result.errors.is_empty(),
        "top-level assignment + expression should parse, got errors: {:?}",
        result.errors,
    );
    let body = result.ast.body.as_ref().expect("expected populated body");
    assert_eq!(body.len(), 2);
    assert!(matches!(body[0], Statement::Assignment { .. }));
    assert!(matches!(body[1], Statement::Expr(_)));
}

#[test]
fn file_mode_after_script_mode_changes_unaffected() {
    let source = "fn main\n  42\nend\n";
    let result = parse(source, ParseMode::File);
    assert!(result.errors.is_empty());
    assert!(result.ast.body.is_none());
    assert_eq!(result.ast.items.len(), 1);
    let Item::Function(function) = &result.ast.items[0] else {
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
