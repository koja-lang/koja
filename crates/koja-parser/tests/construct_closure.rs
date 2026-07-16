//! Coverage for the block-form closure `fn(params) ... end`.
//!
//! The short-closure `expr -> expr` form is covered alongside the
//! Pratt loop in `tests/expr.rs`.

use koja_ast::ast::{ClosureParam, ExprKind, TypeExpr};

mod common;

use common::{assert_hint_contains, first_function_expr, parse_failing_with};

#[test]
fn empty_closure_no_params() {
    let expr = first_function_expr(
        "
        fn run
          c = fn() -> Int
            42
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::Closure {
            params,
            return_type,
            body,
        } => {
            assert!(params.is_empty());
            assert!(matches!(return_type, Some(TypeExpr::Named { .. })));
            assert_eq!(body.len(), 1);
        }
        other => panic!("expected Closure, got {other:?}"),
    }
}

#[test]
fn closure_with_typed_params() {
    let expr = first_function_expr(
        "
        fn run
          add = fn(a: Int, b: Int) -> Int
            a + b
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::Closure { params, .. } => {
            assert_eq!(params.len(), 2);
            for param in &params {
                assert!(matches!(
                    param,
                    ClosureParam::Name {
                        type_expr: Some(_),
                        ..
                    }
                ));
            }
        }
        other => panic!("expected Closure, got {other:?}"),
    }
}

#[test]
fn closure_with_inferred_params() {
    let expr = first_function_expr(
        "
        fn run
          c = fn(x, y)
            x + y
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::Closure { params, .. } => {
            assert_eq!(params.len(), 2);
            for param in &params {
                assert!(matches!(
                    param,
                    ClosureParam::Name {
                        type_expr: None,
                        ..
                    }
                ));
            }
        }
        other => panic!("expected Closure, got {other:?}"),
    }
}

/// Destructured params were removed from the grammar. Re-introduce
/// them if anonymous tuples ever land. Until then `(a, b)` in param
/// position diagnoses with a pointer at named params.
#[test]
fn closure_with_destructured_param_is_diagnosed() {
    let result = parse_failing_with(
        "
        fn run
          c = fn((a, b))
            a + b
          end
        end
        ",
        &["expected closure parameter"],
    );
    assert_hint_contains(&result, "Destructuring is not supported");
}

#[test]
fn closure_without_return_type() {
    let expr = first_function_expr(
        "
        fn run
          c = fn(x)
            x
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::Closure { return_type, .. } => assert!(return_type.is_none()),
        other => panic!("expected Closure, got {other:?}"),
    }
}
