//! Coverage for control-flow expressions: `if`, `unless`, `match`,
//! `cond`, `for`, `loop`, `while`, `receive`.
//!
//! Pins:
//! - block-shape parsing (each form terminates with `end`)
//! - `if`/`else` body presence
//! - `match` arm count, with-and-without `when` guards, or-patterns
//! - `cond` requires `else ->` and rejects without it
//! - `for pattern in iterable` binds correctly
//! - `receive` accepts optional `after timeout` block

use koja_ast::ast::{Expr, ExprKind, Item, Statement};
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_function_expr(source: &str) -> Expr {
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
fn if_with_then_only() {
    let src = dedent(
        "
        fn run
          if x
            1
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::If {
            then_body,
            else_body,
            ..
        } => {
            assert!(!then_body.is_empty());
            assert!(else_body.is_none());
        }
        other => panic!("expected If, got {other:?}"),
    }
}

#[test]
fn if_with_else() {
    let src = dedent(
        "
        fn run
          if x
            1
          else
            2
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::If { else_body, .. } => {
            assert!(else_body.is_some());
        }
        other => panic!("expected If, got {other:?}"),
    }
}

#[test]
fn else_if_is_rejected() {
    let src = dedent(
        "
        fn run
          if x
            1
          else if y
            2
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "else if is not supported");
}

#[test]
fn unless_form() {
    let src = dedent(
        "
        fn run
          unless x
            1
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    assert!(matches!(expr.kind, ExprKind::Unless { .. }));
}

#[test]
fn match_with_arms() {
    let src = dedent(
        "
        fn run
          match x
            0 -> \"zero\"
            1 -> \"one\"
            _ -> \"other\"
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Match { arms, .. } => assert_eq!(arms.len(), 3),
        other => panic!("expected Match, got {other:?}"),
    }
}

#[test]
fn match_arm_with_when_guard() {
    let src = dedent(
        "
        fn run
          match x
            n when n > 0 -> \"pos\"
            _ -> \"other\"
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Match { arms, .. } => {
            assert!(arms[0].guard.is_some());
            assert!(arms[1].guard.is_none());
        }
        other => panic!("expected Match, got {other:?}"),
    }
}

#[test]
fn match_with_or_pattern() {
    let src = dedent(
        "
        fn run
          match x
            1 | 2 | 3 -> \"small\"
            _ -> \"big\"
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Match { arms, .. } => {
            assert!(matches!(arms[0].pattern, koja_ast::ast::Pattern::Or { .. }));
        }
        other => panic!("expected Match, got {other:?}"),
    }
}

#[test]
fn cond_with_else() {
    let src = dedent(
        "
        fn run
          cond
            x > 0 -> \"pos\"
            x < 0 -> \"neg\"
            else -> \"zero\"
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Cond { arms, else_body } => {
            assert_eq!(arms.len(), 2);
            assert!(else_body.is_some());
        }
        other => panic!("expected Cond, got {other:?}"),
    }
}

#[test]
fn cond_without_else_fails() {
    let src = dedent(
        "
        fn run
          cond
            x > 0 -> \"pos\"
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "cond requires an `else ->` arm");
}

#[test]
fn for_loop_with_binding() {
    let src = dedent(
        "
        fn run
          for x in items
            x
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::For { pattern, body, .. } => {
            assert!(matches!(pattern, koja_ast::ast::Pattern::Binding { .. }));
            assert!(!body.is_empty());
        }
        other => panic!("expected For, got {other:?}"),
    }
}

#[test]
fn loop_unbounded() {
    let src = dedent(
        "
        fn run
          loop
            1
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    assert!(matches!(expr.kind, ExprKind::Loop { .. }));
}

#[test]
fn while_loop() {
    let src = dedent(
        "
        fn run
          while x > 0
            x
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    assert!(matches!(expr.kind, ExprKind::While { .. }));
}

#[test]
fn receive_with_arms() {
    let src = dedent(
        "
        fn run
          receive
            x -> 1
            _ -> 0
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Receive {
            arms,
            after_timeout,
            ..
        } => {
            assert_eq!(arms.len(), 2);
            assert!(after_timeout.is_none());
        }
        other => panic!("expected Receive, got {other:?}"),
    }
}

#[test]
fn receive_with_after_clause() {
    let src = dedent(
        "
        fn run
          receive
            x -> 1
          after 1000
            0
          end
        end
        ",
    );
    let expr = first_function_expr(&src);
    match expr.kind {
        ExprKind::Receive {
            after_timeout,
            after_body,
            ..
        } => {
            assert!(after_timeout.is_some());
            assert!(!after_body.is_empty());
        }
        other => panic!("expected Receive, got {other:?}"),
    }
}
