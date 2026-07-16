//! Coverage for bracket and paren grouping expressions: list and
//! map literals, paren expressions, the unit literal `()`, and the
//! "tuples are not supported" rejection path.

use koja_ast::ast::{ExprKind, Literal};

mod common;

use common::{first_function_expr, parse_failing_with};

#[test]
fn empty_list_literal() {
    let expr = first_function_expr(
        "
        fn run
          x = []
        end
        ",
    );
    match expr.kind {
        ExprKind::List { elements } => assert!(elements.is_empty()),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn nonempty_list_literal() {
    let expr = first_function_expr(
        "
        fn run
          x = [1, 2, 3]
        end
        ",
    );
    match expr.kind {
        ExprKind::List { elements } => assert_eq!(elements.len(), 3),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn list_with_trailing_comma() {
    let expr = first_function_expr(
        "
        fn run
          x = [1, 2, 3,]
        end
        ",
    );
    match expr.kind {
        ExprKind::List { elements } => assert_eq!(elements.len(), 3),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn nested_list_literal() {
    let expr = first_function_expr(
        "
        fn run
          x = [[1, 2], [3, 4]]
        end
        ",
    );
    match expr.kind {
        ExprKind::List { elements } => {
            assert_eq!(elements.len(), 2);
            for inner in &elements {
                assert!(matches!(inner.kind, ExprKind::List { .. }));
            }
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn empty_map_literal_uses_colon() {
    let expr = first_function_expr(
        "
        fn run
          x = [:]
        end
        ",
    );
    match expr.kind {
        ExprKind::Map { entries } => assert!(entries.is_empty()),
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn map_literal_with_entries() {
    let expr = first_function_expr(
        "
        fn run
          x = [1: \"one\", 2: \"two\"]
        end
        ",
    );
    match expr.kind {
        ExprKind::Map { entries } => assert_eq!(entries.len(), 2),
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn paren_groups_expression() {
    let expr = first_function_expr(
        "
        fn run
          (1 + 2)
        end
        ",
    );
    assert!(matches!(expr.kind, ExprKind::Group { .. }));
}

#[test]
fn unit_literal_from_empty_parens() {
    let expr = first_function_expr(
        "
        fn run
          ()
        end
        ",
    );
    assert!(matches!(
        expr.kind,
        ExprKind::Literal {
            value: Literal::Unit
        }
    ));
}

#[test]
fn tuple_syntax_is_rejected() {
    parse_failing_with(
        "
        fn run
          (1, 2)
        end
        ",
        &["tuples are not supported"],
    );
}
