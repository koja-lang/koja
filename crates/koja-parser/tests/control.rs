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

use koja_ast::ast::{Expr, ExprKind, Pattern, Statement};

mod common;

use common::{first_function_expr, parse_failing_with};

fn is_closure_assignment(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Assignment {
            value: Expr {
                kind: ExprKind::Closure { .. },
                ..
            },
            ..
        }
    )
}

#[test]
fn if_with_then_only() {
    let expr = first_function_expr(
        "
        fn run
          if x
            1
          end
        end
        ",
    );
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
    let expr = first_function_expr(
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
    match expr.kind {
        ExprKind::If { else_body, .. } => {
            assert!(else_body.is_some());
        }
        other => panic!("expected If, got {other:?}"),
    }
}

#[test]
fn else_if_is_rejected() {
    parse_failing_with(
        "
        fn run
          if x
            1
          else if y
            2
          end
        end
        ",
        &["else if is not supported"],
    );
}

#[test]
fn unless_form() {
    let expr = first_function_expr(
        "
        fn run
          unless x
            1
          end
        end
        ",
    );
    assert!(matches!(expr.kind, ExprKind::Unless { .. }));
}

#[test]
fn match_with_arms() {
    let expr = first_function_expr(
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
    match expr.kind {
        ExprKind::Match { arms, .. } => assert_eq!(arms.len(), 3),
        other => panic!("expected Match, got {other:?}"),
    }
}

#[test]
fn match_arm_with_when_guard() {
    let expr = first_function_expr(
        "
        fn run
          match x
            n when n > 0 -> \"pos\"
            _ -> \"other\"
          end
        end
        ",
    );
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
    let expr = first_function_expr(
        "
        fn run
          match x
            1 | 2 | 3 -> \"small\"
            _ -> \"big\"
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::Match { arms, .. } => {
            assert!(matches!(arms[0].pattern, Pattern::Or { .. }));
        }
        other => panic!("expected Match, got {other:?}"),
    }
}

#[test]
fn cond_with_else() {
    let expr = first_function_expr(
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
    parse_failing_with(
        "
        fn run
          cond
            x > 0 -> \"pos\"
          end
        end
        ",
        &["cond requires an `else ->` arm"],
    );
}

#[test]
fn for_loop_with_binding() {
    let expr = first_function_expr(
        "
        fn run
          for x in items
            x
          end
        end
        ",
    );
    match expr.kind {
        ExprKind::For { pattern, body, .. } => {
            assert!(matches!(pattern, Pattern::Binding { .. }));
            assert!(!body.is_empty());
        }
        other => panic!("expected For, got {other:?}"),
    }
}

#[test]
fn loop_unbounded() {
    let expr = first_function_expr(
        "
        fn run
          loop
            1
          end
        end
        ",
    );
    assert!(matches!(expr.kind, ExprKind::Loop { .. }));
}

#[test]
fn while_loop() {
    let expr = first_function_expr(
        "
        fn run
          while x > 0
            x
          end
        end
        ",
    );
    assert!(matches!(expr.kind, ExprKind::While { .. }));
}

#[test]
fn receive_with_arms() {
    let expr = first_function_expr(
        "
        fn run
          receive
            x -> 1
            _ -> 0
          end
        end
        ",
    );
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
    let expr = first_function_expr(
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

#[test]
fn cond_arm_body_accepts_assigned_block_closure() {
    let expr = first_function_expr(
        "
        fn run
          cond
            ready ->
              transform = fn (x: Int) -> Int
                x * 2
              end
              transform(1)
            else -> 0
          end
        end
        ",
    );
    let ExprKind::Cond { arms, .. } = expr.kind else {
        panic!("expected Cond, got {expr:?}");
    };
    assert_eq!(arms.len(), 1);
    assert!(is_closure_assignment(&arms[0].body[0]));
}

#[test]
fn match_arm_body_accepts_assigned_block_closure() {
    let expr = first_function_expr(
        "
        fn run
          match value
            _ ->
              transform = fn (x: Int) -> Int
                x * 2
              end
              transform(1)
            1 -> 1
          end
        end
        ",
    );
    let ExprKind::Match { arms, .. } = expr.kind else {
        panic!("expected Match, got {expr:?}");
    };
    assert_eq!(arms.len(), 2);
    assert!(is_closure_assignment(&arms[0].body[0]));
}

#[test]
fn match_arm_body_accepts_direct_block_closure() {
    let expr = first_function_expr(
        "
        fn run
          match value
            _ ->
              fn (x: Int) -> Int
                x * 2
              end
            1 -> 1
          end
        end
        ",
    );
    let ExprKind::Match { arms, .. } = expr.kind else {
        panic!("expected Match, got {expr:?}");
    };
    assert_eq!(arms.len(), 2);
    assert!(matches!(
        arms[0].body[0],
        Statement::Expr(Expr {
            kind: ExprKind::Closure { .. },
            ..
        })
    ));
}

#[test]
fn match_arm_body_accepts_short_closure_call_argument() {
    let expr = first_function_expr(
        "
        fn run
          match value
            _ ->
              prepare()
              items.map(x -> x * 2)
            1 -> 1
          end
        end
        ",
    );
    let ExprKind::Match { arms, .. } = expr.kind else {
        panic!("expected Match, got {expr:?}");
    };
    assert_eq!(arms.len(), 2);
    assert_eq!(arms[0].body.len(), 2);
}

#[test]
fn receive_arm_body_accepts_assigned_block_closure() {
    let expr = first_function_expr(
        "
        fn run
          receive
            _ ->
              transform = fn (x: Int) -> Int
                x * 2
              end
              transform(1)
            message -> message
          end
        end
        ",
    );
    let ExprKind::Receive { arms, .. } = expr.kind else {
        panic!("expected Receive, got {expr:?}");
    };
    assert_eq!(arms.len(), 2);
    assert!(is_closure_assignment(&arms[0].body[0]));
}
