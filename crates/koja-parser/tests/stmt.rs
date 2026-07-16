//! Coverage for statement parsing: assignment / typed assignment /
//! compound assignment / `return` / `break`.

use koja_ast::ast::{CompoundOp, ExprKind, Statement, TypeExpr};

mod common;

use common::first_function;

fn function_body(source: &str) -> Vec<Statement> {
    first_function(source).body.unwrap_or_default()
}

#[test]
fn plain_assignment() {
    let body = function_body(
        "
        fn run
          x = 5
        end
        ",
    );
    match &body[0] {
        Statement::Assignment {
            target,
            type_annotation,
            ..
        } => {
            assert!(type_annotation.is_none());
            assert_eq!(target.segments, vec!["x"]);
        }
        other => panic!("expected Assignment, got {other:?}"),
    }
}

#[test]
fn typed_assignment() {
    let body = function_body(
        "
        fn run
          x: Int = 5
        end
        ",
    );
    match &body[0] {
        Statement::Assignment {
            type_annotation, ..
        } => {
            assert!(matches!(
                type_annotation,
                Some(TypeExpr::Named { path, .. }) if path == &vec!["Int".to_string()]
            ));
        }
        other => panic!("expected Assignment, got {other:?}"),
    }
}

#[test]
fn dotted_lvalue_assignment() {
    let body = function_body(
        "
        fn run
          point.x = 5
        end
        ",
    );
    match &body[0] {
        Statement::Assignment { target, .. } => {
            assert_eq!(target.segments, vec!["point", "x"]);
        }
        other => panic!("expected dotted Assignment, got {other:?}"),
    }
}

#[test]
fn compound_add() {
    let body = function_body(
        "
        fn run
          x = 0
          x += 1
        end
        ",
    );
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Add),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_sub() {
    let body = function_body(
        "
        fn run
          x = 0
          x -= 1
        end
        ",
    );
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Sub),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_mul() {
    let body = function_body(
        "
        fn run
          x = 1
          x *= 2
        end
        ",
    );
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Mul),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_div() {
    let body = function_body(
        "
        fn run
          x = 4
          x /= 2
        end
        ",
    );
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Div),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn return_with_value() {
    let body = function_body(
        "
        fn run
          return 42
        end
        ",
    );
    match &body[0] {
        Statement::Return { value, .. } => assert!(value.is_some()),
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn return_without_value() {
    let body = function_body(
        "
        fn run
          return
        end
        ",
    );
    match &body[0] {
        Statement::Return { value, .. } => assert!(value.is_none()),
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn break_statement() {
    let body = function_body(
        "
        fn run
          loop
            break
          end
        end
        ",
    );
    let loop_body = match &body[0] {
        Statement::Expr(expr) => match &expr.kind {
            ExprKind::Loop { body } => body,
            other => panic!("expected Loop, got {other:?}"),
        },
        other => panic!("expected Expr(Loop), got {other:?}"),
    };
    assert!(matches!(loop_body[0], Statement::Break { .. }));
}
