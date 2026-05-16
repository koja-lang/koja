//! Coverage for statement parsing: assignment / typed assignment /
//! compound assignment / `return` / `break` / destructuring
//! assignment.

use expo_ast::ast::{AssignTarget, CompoundOp, Item, Statement, TypeExpr};
use expo_ast::util::dedent;

mod common;

use common::parse_clean;

fn function_body(source: &str) -> Vec<Statement> {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            return f.body.unwrap_or_default();
        }
    }
    panic!("no function in parsed output");
}

#[test]
fn plain_assignment() {
    let src = dedent(
        "
        fn run
          x = 5
        end
        ",
    );
    let body = function_body(&src);
    match &body[0] {
        Statement::Assignment {
            target,
            type_annotation,
            ..
        } => {
            assert!(type_annotation.is_none());
            let lv = match target {
                AssignTarget::LValue(lv) => lv,
                other => panic!("expected LValue target, got {other:?}"),
            };
            assert_eq!(lv.segments, vec!["x"]);
        }
        other => panic!("expected Assignment, got {other:?}"),
    }
}

#[test]
fn typed_assignment() {
    let src = dedent(
        "
        fn run
          x: Int = 5
        end
        ",
    );
    let body = function_body(&src);
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
    let src = dedent(
        "
        fn run
          point.x = 5
        end
        ",
    );
    let body = function_body(&src);
    match &body[0] {
        Statement::Assignment {
            target: AssignTarget::LValue(lv),
            ..
        } => assert_eq!(lv.segments, vec!["point", "x"]),
        other => panic!("expected dotted Assignment, got {other:?}"),
    }
}

#[test]
fn compound_add() {
    let src = dedent(
        "
        fn run
          x = 0
          x += 1
        end
        ",
    );
    let body = function_body(&src);
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Add),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_sub() {
    let src = dedent(
        "
        fn run
          x = 0
          x -= 1
        end
        ",
    );
    let body = function_body(&src);
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Sub),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_mul() {
    let src = dedent(
        "
        fn run
          x = 1
          x *= 2
        end
        ",
    );
    let body = function_body(&src);
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Mul),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn compound_div() {
    let src = dedent(
        "
        fn run
          x = 4
          x /= 2
        end
        ",
    );
    let body = function_body(&src);
    match &body[1] {
        Statement::CompoundAssign { op, .. } => assert_eq!(*op, CompoundOp::Div),
        other => panic!("expected CompoundAssign, got {other:?}"),
    }
}

#[test]
fn return_with_value() {
    let src = dedent(
        "
        fn run
          return 42
        end
        ",
    );
    let body = function_body(&src);
    match &body[0] {
        Statement::Return { value, .. } => assert!(value.is_some()),
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn return_without_value() {
    let src = dedent(
        "
        fn run
          return
        end
        ",
    );
    let body = function_body(&src);
    match &body[0] {
        Statement::Return { value, .. } => assert!(value.is_none()),
        other => panic!("expected Return, got {other:?}"),
    }
}

#[test]
fn break_statement() {
    let src = dedent(
        "
        fn run
          loop
            break
          end
        end
        ",
    );
    let body = function_body(&src);
    let loop_body = match &body[0] {
        Statement::Expr(expr) => match &expr.kind {
            expo_ast::ast::ExprKind::Loop { body } => body,
            other => panic!("expected Loop, got {other:?}"),
        },
        other => panic!("expected Expr(Loop), got {other:?}"),
    };
    assert!(matches!(loop_body[0], Statement::Break { .. }));
}
