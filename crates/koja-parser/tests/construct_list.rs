//! Coverage for bracket and paren grouping expressions: list and
//! map literals, paren expressions, the unit literal `()`, and the
//! "tuples are not supported" rejection path.

use koja_ast::ast::{Expr, ExprKind, Item, Literal, Statement};
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_function_value_expr(source: &str) -> Expr {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            for stmt in f.body.unwrap_or_default() {
                match stmt {
                    Statement::Expr(e) => return e,
                    Statement::Assignment { value, .. } => return value,
                    _ => continue,
                }
            }
        }
    }
    panic!("no expression in parsed output");
}

#[test]
fn empty_list_literal() {
    let src = dedent(
        "
        fn run
          x = []
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    match expr.kind {
        ExprKind::List { elements } => assert!(elements.is_empty()),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn nonempty_list_literal() {
    let src = dedent(
        "
        fn run
          x = [1, 2, 3]
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    match expr.kind {
        ExprKind::List { elements } => assert_eq!(elements.len(), 3),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn list_with_trailing_comma() {
    let src = dedent(
        "
        fn run
          x = [1, 2, 3,]
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    match expr.kind {
        ExprKind::List { elements } => assert_eq!(elements.len(), 3),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn nested_list_literal() {
    let src = dedent(
        "
        fn run
          x = [[1, 2], [3, 4]]
        end
        ",
    );
    let expr = first_function_value_expr(&src);
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
    let src = dedent(
        "
        fn run
          x = [:]
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    match expr.kind {
        ExprKind::Map { entries } => assert!(entries.is_empty()),
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn map_literal_with_entries() {
    let src = dedent(
        "
        fn run
          x = [1: \"one\", 2: \"two\"]
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    match expr.kind {
        ExprKind::Map { entries } => assert_eq!(entries.len(), 2),
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn paren_groups_expression() {
    let src = dedent(
        "
        fn run
          (1 + 2)
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    assert!(matches!(expr.kind, ExprKind::Group { .. }));
}

#[test]
fn unit_literal_from_empty_parens() {
    let src = dedent(
        "
        fn run
          ()
        end
        ",
    );
    let expr = first_function_value_expr(&src);
    assert!(matches!(
        expr.kind,
        ExprKind::Literal {
            value: Literal::Unit
        }
    ));
}

#[test]
fn tuple_syntax_is_rejected() {
    let src = dedent(
        "
        fn run
          (1, 2)
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "tuples are not supported");
}
