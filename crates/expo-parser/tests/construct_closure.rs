//! Coverage for the block-form closure `fn(params) ... end`.
//!
//! The short-closure `expr -> expr` form is covered alongside the
//! Pratt loop in `tests/expr.rs`.

use expo_ast::ast::{ClosureParam, Expr, ExprKind, Item, PassMode, Statement, TypeExpr};
use expo_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_closure_expr(source: &str) -> Expr {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            for stmt in f.body.unwrap_or_default() {
                match stmt {
                    Statement::Expr(e) | Statement::Assignment { value: e, .. } => return e,
                    _ => continue,
                }
            }
        }
    }
    panic!("no expression in parsed output");
}

#[test]
fn empty_closure_no_params() {
    let src = dedent(
        "
        fn run
          c = fn() -> Int
            42
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
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
    let src = dedent(
        "
        fn run
          add = fn(a: Int, b: Int) -> Int
            a + b
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
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
    let src = dedent(
        "
        fn run
          c = fn(x, y)
            x + y
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
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

#[test]
fn closure_with_move_param() {
    let src = dedent(
        "
        fn run
          c = fn(move s: String) -> String
            s
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
    match expr.kind {
        ExprKind::Closure { params, .. } => match &params[0] {
            ClosureParam::Name { mode, .. } => assert_eq!(*mode, PassMode::Move),
            other => panic!("expected Name param, got {other:?}"),
        },
        other => panic!("expected Closure, got {other:?}"),
    }
}

#[test]
fn closure_with_destructured_param() {
    let src = dedent(
        "
        fn run
          c = fn((a, b))
            a + b
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
    match expr.kind {
        ExprKind::Closure { params, .. } => {
            assert_eq!(params.len(), 1);
            match &params[0] {
                ClosureParam::Destructured { names, .. } => {
                    assert_eq!(names, &vec!["a".to_string(), "b".to_string()]);
                }
                other => panic!("expected Destructured, got {other:?}"),
            }
        }
        other => panic!("expected Closure, got {other:?}"),
    }
}

#[test]
fn closure_without_return_type() {
    let src = dedent(
        "
        fn run
          c = fn(x)
            x
          end
        end
        ",
    );
    let expr = first_closure_expr(&src);
    match expr.kind {
        ExprKind::Closure { return_type, .. } => assert!(return_type.is_none()),
        other => panic!("expected Closure, got {other:?}"),
    }
}
