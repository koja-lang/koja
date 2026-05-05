//! Expression-level lowering: dispatch on [`ExprKind`], lower each
//! supported variant into a `(ValueId, IRBlockId)` (the produced
//! value plus the block it sits in), and surface a feature-gap
//! diagnostic for any unsupported variant.
//!
//! [`lower_call`] lives here too because it's the only expression
//! variant that interacts with the [`GlobalRegistry`] beyond the
//! type-side adapters in [`super::package`].

use expo_alpha_typecheck::{GlobalKind, GlobalRegistry};
use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::Resolution;

use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::types::ValueId;

use super::control_flow::{lower_if, lower_unless};
use super::ctx::FnLowerCtx;
use super::ops::{
    bin_op_result_type, const_value_type, lower_bin_op, lower_literal, lower_unary_op,
    unary_op_result_type,
};
use super::package::resolved_type_to_ir_type;

pub(super) fn lower_expr(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    match &expr.kind {
        ExprKind::Binary { op, left, right } => {
            let (lhs, block) = lower_expr(left, ctx, block, registry, diagnostics)?;
            let (rhs, block) = lower_expr(right, ctx, block, registry, diagnostics)?;
            let ir_op = lower_bin_op(*op, expr.span, diagnostics)?;
            let result_ty = bin_op_result_type(ir_op, ctx.type_of(lhs));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest,
                    lhs,
                    op: ir_op,
                    rhs,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, ctx, block, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => lower_expr(inner, ctx, block, registry, diagnostics),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            if else_body.is_some() {
                diagnostics.push(Diagnostic::error(
                    "alpha IR does not yet lower `else` branches",
                    expr.span,
                ));
                return Err(());
            }
            lower_if(condition, then_body, ctx, block, registry, diagnostics)
        }
        ExprKind::Literal { value } => {
            let const_value = lower_literal(value, expr.span, diagnostics)?;
            let ty = const_value_type(&const_value);
            let dest = ctx.fresh_value(ty);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest,
                    value: const_value,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Unary { op, operand } => {
            let (operand, block) = lower_expr(operand, ctx, block, registry, diagnostics)?;
            let ir_op = lower_unary_op(*op);
            let result_ty = unary_op_result_type(ir_op, ctx.type_of(operand));
            let dest = ctx.fresh_value(result_ty);
            ctx.cfg.append(
                block,
                IRInstruction::UnaryOp {
                    dest,
                    op: ir_op,
                    operand,
                },
            );
            Ok((dest, block))
        }
        ExprKind::Unless { condition, body } => {
            lower_unless(condition, body, ctx, block, registry, diagnostics)
        }
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower this expression kind ({})",
                    expr_kind_label(other),
                ),
                expr.span,
            ));
            Err(())
        }
    }
}

/// Lower a `ExprKind::Call`. The seal contract guarantees the callee
/// is a bare `Ident` whose inner `Resolution` is `Global(id)` — any
/// deviation is a compiler bug, not a feature gap, so we panic rather
/// than emit a diagnostic.
fn lower_call(
    callee: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let ExprKind::Ident { resolution, name } = &callee.kind else {
        panic!(
            "alpha IR lower: call callee must be a bare Ident after typecheck seal (got {:?})",
            callee.kind,
        );
    };
    let Resolution::Global(id) = resolution else {
        panic!("alpha IR lower: callee `{name}` has Unresolved resolution after typecheck seal",);
    };
    let entry = registry.get(*id).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: callee id {id} not present in the registry — \
             seal invariant violation",
        )
    });
    let signature = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: callee `{}` resolved to non-function entry ({}) — \
             typecheck seal violation",
            entry.identifier,
            other.label(),
        ),
    };
    let return_ty = resolved_type_to_ir_type(&signature.return_type, registry);
    let callee_symbol = IRSymbol::from_identifier(&entry.identifier);

    let mut lowered_args = Vec::with_capacity(args.len());
    let mut current = block;
    for arg in args {
        let (value, next) = lower_expr(&arg.value, ctx, current, registry, diagnostics)?;
        lowered_args.push(value);
        current = next;
    }

    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        current,
        IRInstruction::Call {
            dest,
            callee: callee_symbol,
            args: lowered_args,
        },
    );
    Ok((dest, current))
}

/// Short, user-facing label for an [`ExprKind`] that the alpha IR
/// cannot yet lower. Kept local because it only serves feature-gap
/// diagnostics; a public `ExprKind::label()` would imply stability
/// guarantees we aren't ready to make.
fn expr_kind_label(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Binary { .. } => "binary expression",
        ExprKind::BinaryLiteral { .. } => "binary literal",
        ExprKind::Call { .. } => "call",
        ExprKind::Closure { .. } => "closure",
        ExprKind::Cond { .. } => "cond",
        ExprKind::EnumConstruction { .. } => "enum construction",
        ExprKind::FieldAccess { .. } => "field access",
        ExprKind::For { .. } => "for",
        ExprKind::Group { .. } => "group",
        ExprKind::Ident { .. } => "identifier reference",
        ExprKind::If { .. } => "if",
        ExprKind::List { .. } => "list literal",
        ExprKind::Literal { .. } => "literal",
        ExprKind::Loop { .. } => "loop",
        ExprKind::Map { .. } => "map literal",
        ExprKind::Match { .. } => "match",
        ExprKind::MethodCall { .. } => "method call",
        ExprKind::Receive { .. } => "receive",
        ExprKind::Self_ => "self reference",
        ExprKind::ShortClosure { .. } => "short closure",
        ExprKind::Spawn { .. } => "spawn",
        ExprKind::String { .. } => "string interpolation",
        ExprKind::StructConstruction { .. } => "struct construction",
        ExprKind::Ternary { .. } => "ternary",
        ExprKind::Unary { .. } => "unary",
        ExprKind::Unless { .. } => "unless",
        ExprKind::While { .. } => "while",
    }
}
