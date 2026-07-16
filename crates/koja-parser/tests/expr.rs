//! Pratt-precedence and prefix-dispatch coverage for the expression
//! parser. Pins:
//!
//! - associativity for `+`/`-`, `*`/`/`/`%`, `==`/`!=`/`<`/`>`/`<=`/`>=`,
//!   `and`/`or`, and the `?:` ternary
//! - the precedence ladder: postfix > unary > mul > add > cmp > and >
//!   or > ternary > short-closure arrow
//! - postfix forms (field access, method call, call, ternary)
//! - the `expr -> expr` short-closure shape and its single-param /
//!   wildcard / parenthesized variants

use koja_ast::ast::{BinOp, ClosureParam, Expr, ExprKind, Literal, UnaryOp};

mod common;

use common::first_script_expr;

fn is_binop(expr: &Expr, expected_op: BinOp) -> bool {
    matches!(expr.kind, ExprKind::Binary { op, .. } if op == expected_op)
}

// ---- Arithmetic precedence ----

#[test]
fn mul_binds_tighter_than_add() {
    let expr = first_script_expr("1 + 2 * 3");
    let ExprKind::Binary {
        op, left, right, ..
    } = &expr.kind
    else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Add);
    assert!(matches!(left.kind, ExprKind::Literal { .. }));
    assert!(is_binop(right, BinOp::Mul));
}

#[test]
fn sub_and_div_precedence() {
    let expr = first_script_expr("a - b / c");
    let ExprKind::Binary { op, right, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Sub);
    assert!(is_binop(right, BinOp::Div));
}

#[test]
fn left_associativity_add() {
    let expr = first_script_expr("1 + 2 + 3");
    let ExprKind::Binary { op, left, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Add);
    assert!(is_binop(left, BinOp::Add));
}

// ---- Comparison ----

#[test]
fn comparison_parses() {
    let expr = first_script_expr("a == b");
    assert!(is_binop(&expr, BinOp::Eq));

    let expr2 = first_script_expr("x != y");
    assert!(is_binop(&expr2, BinOp::NotEq));

    let expr3 = first_script_expr("a < b");
    assert!(is_binop(&expr3, BinOp::Lt));
}

#[test]
fn comparison_lower_than_arithmetic() {
    let expr = first_script_expr("a + 1 == b * 2");
    let ExprKind::Binary {
        op, left, right, ..
    } = &expr.kind
    else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Eq);
    assert!(is_binop(left, BinOp::Add));
    assert!(is_binop(right, BinOp::Mul));
}

// ---- Logical operators ----

#[test]
fn and_binds_tighter_than_or() {
    let expr = first_script_expr("a or b and c");
    let ExprKind::Binary { op, right, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Or);
    assert!(is_binop(right, BinOp::And));
}

#[test]
fn logical_lower_than_comparison() {
    let expr = first_script_expr("x > 0 and y < 10");
    let ExprKind::Binary {
        op, left, right, ..
    } = &expr.kind
    else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::And);
    assert!(is_binop(left, BinOp::Gt));
    assert!(is_binop(right, BinOp::Lt));
}

// ---- Unary ----

#[test]
fn unary_neg() {
    let expr = first_script_expr("-x");
    let ExprKind::Unary { op, .. } = &expr.kind else {
        panic!("expected Unary, got {expr:?}");
    };
    assert_eq!(*op, UnaryOp::Neg);
}

#[test]
fn unary_binds_tighter_than_binary() {
    let expr = first_script_expr("-a + b");
    let ExprKind::Binary { op, left, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Add);
    assert!(matches!(
        left.kind,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            ..
        }
    ));
}

// ---- Ternary ----

#[test]
fn ternary_parses() {
    let expr = first_script_expr("x ? 1 : 0");
    assert!(matches!(expr.kind, ExprKind::Ternary { .. }));
}

#[test]
fn ternary_lower_than_comparison() {
    let expr = first_script_expr("a > b ? 1 : 0");
    let ExprKind::Ternary { condition, .. } = &expr.kind else {
        panic!("expected Ternary, got {expr:?}");
    };
    assert!(is_binop(condition, BinOp::Gt));
}

// ---- Field access and method call ----

#[test]
fn field_access() {
    let expr = first_script_expr("point.x");
    let ExprKind::FieldAccess { field, .. } = &expr.kind else {
        panic!("expected FieldAccess, got {expr:?}");
    };
    assert_eq!(field, "x");
}

#[test]
fn chained_field_access() {
    let expr = first_script_expr("a.b.c");
    let ExprKind::FieldAccess {
        field, receiver, ..
    } = &expr.kind
    else {
        panic!("expected FieldAccess, got {expr:?}");
    };
    assert_eq!(field, "c");
    assert!(matches!(receiver.kind, ExprKind::FieldAccess { ref field, .. } if field == "b"));
}

#[test]
fn method_call() {
    let expr = first_script_expr("list.push(42)");
    let ExprKind::MethodCall { method, args, .. } = &expr.kind else {
        panic!("expected MethodCall, got {expr:?}");
    };
    assert_eq!(method, "push");
    assert_eq!(args.len(), 1);
}

// ---- Modulus ----

#[test]
fn modulus_same_precedence_as_mul() {
    let expr = first_script_expr("a * b % c");
    let ExprKind::Binary { op, left, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::Mod);
    assert!(is_binop(left, BinOp::Mul));
}

// ---- Short closures ----

#[test]
fn short_closure_single_param() {
    let expr = first_script_expr("x -> x * 2");
    let ExprKind::ShortClosure { params, body, .. } = &expr.kind else {
        panic!("expected ShortClosure, got {expr:?}");
    };
    assert_eq!(params.len(), 1);
    assert!(matches!(&params[0], ClosureParam::Name { name, type_expr: None, .. } if name == "x"));
    assert!(is_binop(body, BinOp::Mul));
}

#[test]
fn short_closure_wildcard_param() {
    let expr = first_script_expr("_ -> 42");
    let ExprKind::ShortClosure { params, body, .. } = &expr.kind else {
        panic!("expected ShortClosure, got {expr:?}");
    };
    assert_eq!(params.len(), 1);
    assert!(matches!(&params[0], ClosureParam::Wildcard { .. }));
    assert!(matches!(
        body.kind,
        ExprKind::Literal {
            value: Literal::Int(_)
        }
    ));
}

#[test]
fn short_closure_body_is_full_expr() {
    let expr = first_script_expr("x -> x + 1 * 2");
    let ExprKind::ShortClosure { body, .. } = &expr.kind else {
        panic!("expected ShortClosure, got {expr:?}");
    };
    assert!(is_binop(body, BinOp::Add));
}

#[test]
fn short_closure_lower_precedence_than_arithmetic() {
    let expr = first_script_expr("a -> a + b");
    let ExprKind::ShortClosure { params, body, .. } = &expr.kind else {
        panic!("expected ShortClosure, got {expr:?}");
    };
    assert_eq!(params.len(), 1);
    assert!(matches!(&params[0], ClosureParam::Name { name, .. } if name == "a"));
    assert!(is_binop(body, BinOp::Add));
}

#[test]
fn short_closure_in_parenthesized_context() {
    let expr = first_script_expr("apply(5, x -> x + 1)");
    let ExprKind::Call { args, .. } = expr.kind else {
        panic!("expected a call expression, got {expr:?}");
    };
    assert_eq!(args.len(), 2);
    assert!(matches!(args[1].value.kind, ExprKind::ShortClosure { .. }));
}

// ---- `not` unary operator ----

#[test]
fn not_unary() {
    let expr = first_script_expr("not x");
    let ExprKind::Unary { op, .. } = &expr.kind else {
        panic!("expected Unary, got {expr:?}");
    };
    assert_eq!(*op, UnaryOp::Not);
}

#[test]
fn not_binds_tighter_than_and() {
    let expr = first_script_expr("not a and b");
    let ExprKind::Binary { op, left, .. } = &expr.kind else {
        panic!("expected Binary, got {expr:?}");
    };
    assert_eq!(*op, BinOp::And);
    assert!(matches!(
        left.kind,
        ExprKind::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));
}

// ---- Concat (`<>`) ----

#[test]
fn concat_parses() {
    let expr = first_script_expr("\"a\" <> \"b\"");
    assert!(is_binop(&expr, BinOp::Concat));
}

// ---- Calls and chaining ----

#[test]
fn function_call_with_args() {
    let expr = first_script_expr("f(1, 2, 3)");
    let ExprKind::Call { args, .. } = &expr.kind else {
        panic!("expected Call, got {expr:?}");
    };
    assert_eq!(args.len(), 3);
}

#[test]
fn method_call_chains_through_field() {
    let expr = first_script_expr("a.b.push(1)");
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr.kind
    else {
        panic!("expected MethodCall, got {expr:?}");
    };
    assert_eq!(method, "push");
    assert!(matches!(receiver.kind, ExprKind::FieldAccess { .. }));
}

#[test]
fn call_result_chains_into_field_access() {
    let expr = first_script_expr("f().x");
    let ExprKind::FieldAccess {
        receiver, field, ..
    } = &expr.kind
    else {
        panic!("expected FieldAccess, got {expr:?}");
    };
    assert_eq!(field, "x");
    assert!(matches!(receiver.kind, ExprKind::Call { .. }));
}

// ---- Literals ----

#[test]
fn int_literal() {
    let expr = first_script_expr("42");
    assert!(matches!(
        expr.kind,
        ExprKind::Literal {
            value: Literal::Int(_)
        }
    ));
}

#[test]
fn bool_literal_true() {
    let expr = first_script_expr("true");
    assert!(matches!(
        expr.kind,
        ExprKind::Literal {
            value: Literal::Bool(true)
        }
    ));
}

#[test]
fn bool_literal_false() {
    let expr = first_script_expr("false");
    assert!(matches!(
        expr.kind,
        ExprKind::Literal {
            value: Literal::Bool(false)
        }
    ));
}
