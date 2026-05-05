//! Type rules for literal / binary / unary expressions.
//!
//! Every helper is registry-backed: outputs flow through
//! [`GlobalRegistry::primitive`] so primitive identity stays
//! single-sourced. On a type mismatch we emit a diagnostic and return
//! [`ResolvedType::unresolved`]; resolve never aborts mid-walk, so a
//! follow-on type rule sees `<unresolved>` operands and stays quiet
//! ([`super::types::is_primitive`] short-circuits on those).

use expo_ast::ast::{BinOp, Diagnostic, Expr, Literal, UnaryOp};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use crate::labels::bin_op_label;
use crate::registry::GlobalRegistry;

use super::types::{display_resolution, is_primitive};

pub(super) fn literal_type(value: &Literal, registry: &GlobalRegistry) -> ResolvedType {
    match value {
        Literal::Bool(_) => registry.primitive("Bool"),
        Literal::Float(_) => registry.primitive("Float"),
        Literal::Int(_) => registry.primitive("Int"),
        Literal::String(_) => registry.primitive("String"),
        Literal::Unit => registry.primitive("Unit"),
    }
}

pub(super) fn binary_type(
    op: BinOp,
    left: &Expr,
    right: &Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let lhs = &left.resolution;
    let rhs = &right.resolution;
    match op {
        BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
            // Arithmetic preserves the operand's numeric width: both
            // sides must agree on `Int` or `Float`. Cross-numeric
            // mixing (`1 + 1.0`) is an error per `LANGUAGE.md`.
            if both(lhs, rhs, registry, "Int") {
                registry.primitive("Int")
            } else if both(lhs, rhs, registry, "Float") {
                registry.primitive("Float")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "Int or Float operands of the same type",
                    lhs,
                    rhs,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::And | BinOp::Or => {
            if both(lhs, rhs, registry, "Bool") {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(diagnostics, op, "Bool operands", lhs, rhs, span, registry);
                ResolvedType::unresolved()
            }
        }
        BinOp::Eq | BinOp::NotEq => {
            if both(lhs, rhs, registry, "Bool")
                || both(lhs, rhs, registry, "Float")
                || both(lhs, rhs, registry, "Int")
            {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "matching Bool, Float, or Int operands",
                    lhs,
                    rhs,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::Gt | BinOp::GtEq | BinOp::Lt | BinOp::LtEq => {
            if both(lhs, rhs, registry, "Float") || both(lhs, rhs, registry, "Int") {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "Int or Float operands of the same type",
                    lhs,
                    rhs,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck does not yet support binary operator `{op:?}`"),
                span,
            ));
            ResolvedType::unresolved()
        }
    }
}

pub(super) fn unary_type(
    op: UnaryOp,
    operand: &Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = &operand.resolution;
    match op {
        UnaryOp::Neg => {
            if is_primitive(ty, registry, "Int") {
                registry.primitive("Int")
            } else if is_primitive(ty, registry, "Float") {
                registry.primitive("Float")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "unary `-` requires an Int or Float operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        UnaryOp::Not => {
            if is_primitive(ty, registry, "Bool") {
                registry.primitive("Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`not` requires a Bool operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
    }
}

fn both(lhs: &ResolvedType, rhs: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    is_primitive(lhs, registry, name) && is_primitive(rhs, registry, name)
}

fn push_op_mismatch(
    diagnostics: &mut Vec<Diagnostic>,
    op: BinOp,
    expected: &str,
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "`{}` requires {expected}; got `{}` and `{}`",
            bin_op_label(op),
            display_resolution(lhs, registry),
            display_resolution(rhs, registry),
        ),
        span,
    ));
}
