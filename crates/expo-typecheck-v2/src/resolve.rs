//! Resolve sub-pass: walk every body in `file`, populating `Resolution`
//! on identifier references and `Expr.resolved_type` on every expression.
//!
//! The POC scope is `2 + 2`, so the only expression shapes that need
//! real handling are integer literals (resolve to `Type::Primitive(I64)`)
//! and `Binary { Add, .. }` (resolve to the integer type that flows
//! through the operands). Identifier references and richer shapes land
//! when [`crate::lift_signatures`] gains a real implementation.

use expo_ast::ast::{BinOp, Diagnostic, Expr, ExprKind, File, Function, Item, Literal, Statement};
use expo_ast::span::Span;
use expo_ast::types::{Primitive, Type};

use crate::labels::expr_kind_label;
use crate::registry::GlobalRegistry;

pub(crate) fn resolve_file(
    file: &mut File,
    _registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // `_registry` is unused for the POC scope (`2 + 2` has no `Ident`
    // references), but kept on the entry point so the orchestration in
    // `program.rs` stays uniform with the other passes. It is plumbed
    // back through the resolve helpers when identifier handling lands.
    for item in &mut file.items {
        if let Item::Function(function) = item {
            resolve_function(function, diagnostics);
        }
    }
}

fn resolve_function(function: &mut Function, diagnostics: &mut Vec<Diagnostic>) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    for stmt in body.iter_mut() {
        resolve_statement(stmt, diagnostics);
    }
}

fn resolve_statement(stmt: &mut Statement, diagnostics: &mut Vec<Diagnostic>) {
    match stmt {
        Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
            resolve_expr(value, diagnostics);
        }
        Statement::Break { .. } => {}
        Statement::Expr(expr) => {
            resolve_expr(expr, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expr(value, diagnostics);
            }
        }
    }
}

fn resolve_expr(expr: &mut Expr, diagnostics: &mut Vec<Diagnostic>) {
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, diagnostics);
            resolve_expr(right, diagnostics);
            binary_type(*op, left, right, expr.span, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, diagnostics);
            inner.resolved_type.clone().unwrap_or(Type::Unknown)
        }
        ExprKind::Literal { value } => literal_type(value),
        // Anything else: emit a diagnostic and mark Unknown. The POC
        // does not need to support these shapes; they unblock as
        // features land.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "v2 typecheck POC does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            Type::Unknown
        }
    };
    expr.resolved_type = Some(ty);
}

fn literal_type(value: &Literal) -> Type {
    match value {
        Literal::Bool(_) => Type::Primitive(Primitive::Bool),
        Literal::Float(_) => Type::Primitive(Primitive::F64),
        Literal::Int(_) => Type::Primitive(Primitive::I64),
        Literal::String(_) => Type::Primitive(Primitive::String),
        Literal::Unit => Type::Unit,
    }
}

fn binary_type(
    op: BinOp,
    left: &Expr,
    right: &Expr,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let lhs = left.resolved_type.clone().unwrap_or(Type::Unknown);
    let rhs = right.resolved_type.clone().unwrap_or(Type::Unknown);
    match op {
        BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
            if matches!(&lhs, Type::Primitive(p) if p.is_integer())
                && matches!(&rhs, Type::Primitive(p) if p.is_integer())
            {
                lhs
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "v2 typecheck POC supports integer arithmetic only; got `{}` and `{}`",
                        lhs.display(),
                        rhs.display()
                    ),
                    span,
                ));
                Type::Unknown
            }
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                format!("v2 typecheck POC does not yet support binary operator `{op:?}`"),
                span,
            ));
            Type::Unknown
        }
    }
}
