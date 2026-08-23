//! Contextual scalar literal conversion through literal protocols.

use koja_ast::ast::{Diagnostic, Expr, ExprKind, Literal, UnaryOp};
use koja_ast::identifier::ResolvedType;

use super::super::coercion::check_float_literal_finite;
use super::super::ctx::Resolver;
use super::super::expr::resolve_expr;
use super::super::ops::unary_type;
use super::super::strings::resolve_string;
use super::carrier::{
    CarrierSpec, Dispatch, dispatch_via_carrier, lookup_global_id, missing_root_diagnostic,
    pick_carrier,
};

const BOOL_SPEC: CarrierSpec = CarrierSpec {
    root_name: "Bool",
    protocol_name: "BoolLiteral",
    from_method: "from_bool",
    missing_root_label: "boolean literal",
};

const FLOAT_SPEC: CarrierSpec = CarrierSpec {
    root_name: "Float",
    protocol_name: "FloatLiteral",
    from_method: "from_float",
    missing_root_label: "float literal",
};

const INT_SPEC: CarrierSpec = CarrierSpec {
    root_name: "Int",
    protocol_name: "IntLiteral",
    from_method: "from_int",
    missing_root_label: "integer literal",
};

const STRING_SPEC: CarrierSpec = CarrierSpec {
    root_name: "String",
    protocol_name: "StringLiteral",
    from_method: "from_string",
    missing_root_label: "string literal",
};

pub(in super::super) fn is_scalar_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Literal {
            value: Literal::Bool(_) | Literal::Float(_) | Literal::Int(_) | Literal::String(_),
        }
        | ExprKind::String { .. } => true,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => matches!(
            &operand.kind,
            ExprKind::Literal {
                value: Literal::Float(_) | Literal::Int(_)
            }
        ),
        _ => false,
    }
}

pub(in super::super) fn resolve_scalar_literal(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if expr.resolution.is_resolved() {
        return expr.resolution.clone();
    }

    let spec = scalar_spec(expr);
    let canonical_type = resolve_canonical(expr, resolver, diagnostics);
    if !canonical_type.is_resolved() {
        return canonical_type;
    }
    let Some(root_id) = lookup_global_id(resolver, spec.root_name) else {
        return missing_root_diagnostic(spec, expr.span, diagnostics);
    };
    let carrier = pick_carrier(expected, root_id, spec, resolver);
    let inner_kind = std::mem::replace(
        &mut expr.kind,
        ExprKind::Literal {
            value: Literal::Unit,
        },
    );
    dispatch_via_carrier(
        expr,
        inner_kind,
        canonical_type,
        &Dispatch {
            expected,
            carrier,
            spec,
        },
        resolver,
        diagnostics,
    )
}

fn scalar_spec(expr: &Expr) -> &'static CarrierSpec {
    match &expr.kind {
        ExprKind::Literal {
            value: Literal::Bool(_),
        } => &BOOL_SPEC,
        ExprKind::Literal {
            value: Literal::Float(_),
        } => &FLOAT_SPEC,
        ExprKind::Literal {
            value: Literal::Int(_),
        } => &INT_SPEC,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Literal {
                value: Literal::Float(_),
            } => &FLOAT_SPEC,
            ExprKind::Literal {
                value: Literal::Int(_),
            } => &INT_SPEC,
            _ => unreachable!("negated scalar literal has a non-numeric operand"),
        },
        ExprKind::Literal {
            value: Literal::String(_),
        }
        | ExprKind::String { .. } => &STRING_SPEC,
        _ => unreachable!("resolve_scalar_literal called for a non-scalar literal"),
    }
}

fn resolve_canonical(
    expr: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match &mut expr.kind {
        ExprKind::Literal { value } => {
            check_float_literal_finite(value, expr.span, diagnostics);
            resolver.registry.literal_type(value)
        }
        ExprKind::String { parts, .. } => resolve_string(parts, expr.span, resolver, diagnostics),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            resolve_expr(operand, resolver, diagnostics);
            unary_type(
                UnaryOp::Neg,
                operand,
                expr.span,
                resolver.registry,
                diagnostics,
            )
        }
        _ => unreachable!("resolve_scalar_literal called for a non-scalar literal"),
    }
}
