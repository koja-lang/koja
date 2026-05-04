//! Resolve sub-pass: walk every body in `file`, populating `Resolution`
//! on identifier references and `Expr.resolved_type` on every expression.
//!
//! The POC scope covers integer arithmetic, the boolean operators
//! (`and`, `or`, `not`), and the comparison operators
//! (`== != < > <= >=`). Identifier references and richer shapes land
//! when a future `lift_signatures` pass starts publishing resolved
//! signatures the resolver can look up.

use expo_ast::ast::{
    BinOp, Diagnostic, Expr, ExprKind, File, Function, Item, Literal, Statement, UnaryOp,
};
use expo_ast::span::Span;
use expo_ast::types::{Primitive, Type};

use crate::labels::expr_kind_label;

pub(crate) fn resolve_file(file: &mut File, diagnostics: &mut Vec<Diagnostic>) {
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
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, diagnostics);
            unary_type(*op, operand, expr.span, diagnostics)
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
                    "alpha typecheck POC does not yet support expression `{}`",
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
            if is_int(&lhs) && is_int(&rhs) {
                lhs
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck POC supports integer arithmetic only; got `{}` and `{}`",
                        lhs.display(),
                        rhs.display()
                    ),
                    span,
                ));
                Type::Unknown
            }
        }
        BinOp::And | BinOp::Or => {
            if is_bool(&lhs) && is_bool(&rhs) {
                Type::Primitive(Primitive::Bool)
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires Bool operands; got `{}` and `{}`",
                        bin_op_label(op),
                        lhs.display(),
                        rhs.display()
                    ),
                    span,
                ));
                Type::Unknown
            }
        }
        BinOp::Eq | BinOp::NotEq => {
            let matches = (is_int(&lhs) && is_int(&rhs)) || (is_bool(&lhs) && is_bool(&rhs));
            if matches {
                Type::Primitive(Primitive::Bool)
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires matching Int or Bool operands; got `{}` and `{}`",
                        bin_op_label(op),
                        lhs.display(),
                        rhs.display()
                    ),
                    span,
                ));
                Type::Unknown
            }
        }
        BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
            if is_int(&lhs) && is_int(&rhs) {
                Type::Primitive(Primitive::Bool)
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires Int operands; got `{}` and `{}`",
                        bin_op_label(op),
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
                format!("alpha typecheck POC does not yet support binary operator `{op:?}`"),
                span,
            ));
            Type::Unknown
        }
    }
}

fn unary_type(op: UnaryOp, operand: &Expr, span: Span, diagnostics: &mut Vec<Diagnostic>) -> Type {
    let ty = operand.resolved_type.clone().unwrap_or(Type::Unknown);
    match op {
        UnaryOp::Not => {
            if is_bool(&ty) {
                Type::Primitive(Primitive::Bool)
            } else {
                diagnostics.push(Diagnostic::error(
                    format!("`not` requires a Bool operand; got `{}`", ty.display()),
                    span,
                ));
                Type::Unknown
            }
        }
        UnaryOp::Neg => {
            if is_int(&ty) {
                ty
            } else {
                diagnostics.push(Diagnostic::error(
                    format!("unary `-` requires an Int operand; got `{}`", ty.display()),
                    span,
                ));
                Type::Unknown
            }
        }
    }
}

fn is_int(ty: &Type) -> bool {
    matches!(ty, Type::Primitive(p) if p.is_integer())
}

fn is_bool(ty: &Type) -> bool {
    matches!(ty, Type::Primitive(Primitive::Bool))
}

fn bin_op_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::And => "and",
        BinOp::Concat => "<>",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Mod => "%",
        BinOp::Mul => "*",
        BinOp::NotEq => "!=",
        BinOp::Or => "or",
        BinOp::Sub => "-",
    }
}
