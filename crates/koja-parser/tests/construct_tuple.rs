//! Coverage for anonymous tuples: literals, tuple type annotations,
//! destructuring assignment, tuple match patterns, and the
//! trailing-comma / refutable-element rejection paths.

use koja_ast::ast::{ExprKind, Pattern, Statement, TypeExpr};

mod common;

use common::{
    assert_hint_contains, first_function, first_function_expr, first_match_arms, parse_failing_with,
};

fn function_body(source: &str) -> Vec<Statement> {
    first_function(source).body.unwrap_or_default()
}

#[test]
fn tuple_literal() {
    let expr = first_function_expr(
        "
        fn run
          x = (1, \"two\")
        end
        ",
    );
    match expr.kind {
        ExprKind::Tuple { elements } => assert_eq!(elements.len(), 2),
        other => panic!("expected Tuple, got {other:?}"),
    }
}

#[test]
fn nested_tuple_literal() {
    let expr = first_function_expr(
        "
        fn run
          x = (1, (2, 3))
        end
        ",
    );
    let ExprKind::Tuple { elements } = expr.kind else {
        panic!("expected Tuple, got {:?}", expr.kind);
    };
    assert!(matches!(elements[1].kind, ExprKind::Tuple { .. }));
}

#[test]
fn single_element_parens_stay_a_group() {
    let expr = first_function_expr(
        "
        fn run
          x = (1)
        end
        ",
    );
    assert!(!matches!(expr.kind, ExprKind::Tuple { .. }));
}

#[test]
fn tuple_literal_rejects_trailing_comma() {
    parse_failing_with(
        "
        fn run
          x = (1, 2,)
        end
        ",
        &["tuples do not allow trailing commas"],
    );
}

#[test]
fn tuple_type_annotation() {
    let body = function_body(
        "
        fn run
          x: (Int, String) = (1, \"two\")
        end
        ",
    );
    let Statement::Assignment {
        type_annotation: Some(annotation),
        ..
    } = &body[0]
    else {
        panic!("expected annotated Assignment, got {:?}", body[0]);
    };
    let TypeExpr::Tuple { elements, .. } = annotation else {
        panic!("expected Tuple type, got {annotation:?}");
    };
    assert_eq!(elements.len(), 2);
}

#[test]
fn tuple_type_rejects_trailing_comma() {
    parse_failing_with(
        "
        fn run
          x: (Int, String,) = (1, \"two\")
        end
        ",
        &["tuple types do not allow trailing commas"],
    );
}

#[test]
fn destructure_statement() {
    let body = function_body(
        "
        fn run
          (a, _, (b, c)) = value()
        end
        ",
    );
    let Statement::Destructure { pattern, .. } = &body[0] else {
        panic!("expected Destructure, got {:?}", body[0]);
    };
    let Pattern::Tuple { elements, .. } = pattern else {
        panic!("expected Tuple pattern, got {pattern:?}");
    };
    assert_eq!(elements.len(), 3);
    assert!(matches!(elements[0], Pattern::Binding { .. }));
    assert!(matches!(elements[1], Pattern::Wildcard { .. }));
    assert!(matches!(elements[2], Pattern::Tuple { .. }));
}

#[test]
fn destructure_rejects_refutable_elements() {
    let result = parse_failing_with(
        "
        fn run
          (a, 1) = value()
        end
        ",
        &["invalid destructuring target"],
    );
    assert_hint_contains(&result, "only names, `_`, and nested tuples");
}

#[test]
fn tuple_match_pattern() {
    let arms = first_match_arms(
        "
        fn run
          match pair
            (1, second) -> second
            (_, _) -> 0
          end
        end
        ",
    );
    let Pattern::Tuple { elements, .. } = &arms[0].pattern else {
        panic!("expected Tuple pattern, got {:?}", arms[0].pattern);
    };
    assert!(matches!(elements[0], Pattern::Literal { .. }));
    assert!(matches!(elements[1], Pattern::Binding { .. }));
}

#[test]
fn tuple_pattern_rejects_trailing_comma() {
    parse_failing_with(
        "
        fn run
          match pair
            (a, b,) -> a
          end
        end
        ",
        &["tuples do not allow trailing commas"],
    );
}
