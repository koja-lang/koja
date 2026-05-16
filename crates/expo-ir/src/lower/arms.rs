//! Shared arm-lowering helpers used by `if` / `cond` / `unless` /
//! ternary / `match`. Each construct synthesizes a merge block with
//! one [`crate::function::BlockParam`] per join and routes its
//! reaching arms here so coercion + the fall-through `Branch`
//! terminator land in one place.
//!
//! Pulled out of [`super::control_flow`] so [`super::match_expr`]
//! can reuse the same exit pattern without re-importing private
//! helpers.

use expo_ast::ast::{Expr, Statement};
use expo_ast::identifier::ResolvedType;
use expo_typecheck::GlobalRegistry;

use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

use super::body::lower_body;
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;

/// Lower a statement-body arm into `arm_block`, then unconditionally
/// jump to `merge_block` when flow stays open. Closed flow (early
/// `return`) leaves the existing terminator in place; the merge
/// block tolerates one fewer incoming edge.
pub(super) fn lower_arm_into(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    arm_block: IRBlockId,
    merge_block: IRBlockId,
    result_ty: &IRType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(), ()> {
    match lower_body(body, ctx, arm_block, registry, output)? {
        FlowResult::Open { block, value } => {
            let arg = match value {
                Some(id) => coerce_arm_value(ctx, block, id, result_ty),
                None => emit_unit(ctx, block),
            };
            ctx.cfg.set_terminator(
                block,
                IRTerminator::Branch(BranchTarget::with_args(merge_block, vec![arg])),
            );
        }
        FlowResult::Closed => {}
    }
    Ok(())
}

/// Lower an expression-shaped arm (ternary today; could grow more
/// callers later) into `arm_block`, then unconditionally jump to
/// `merge_block`. Mirrors [`lower_arm_into`] minus the `lower_body` /
/// `FlowResult` machinery — an `Expr` arm cannot syntactically
/// `return`.
pub(super) fn lower_expr_arm_into(
    expr: &Expr,
    ctx: &mut FnLowerCtx,
    arm_block: IRBlockId,
    merge_block: IRBlockId,
    result_ty: &IRType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(), ()> {
    let (value, after) = lower_expr(expr, ctx, arm_block, registry, output)?;
    let arg = coerce_arm_value(ctx, after, value, result_ty);
    ctx.cfg.set_terminator(
        after,
        IRTerminator::Branch(BranchTarget::with_args(merge_block, vec![arg])),
    );
    Ok(())
}

/// Conform an arm tail value to the merge block's `BlockParam` type.
/// Two cases surface today:
///
/// - Identity match (the arm produced a value of `result_ty`):
///   no-op, the value flows through.
/// - `result_ty == Unit` and the arm produced something else:
///   substitute a fresh `Const::Unit` so the merge edge stays
///   type-consistent (a no-else `if`'s then-arm tails on a non-Unit
///   value the surrounding expression types as `Unit`).
///
/// Other mismatches indicate a typecheck/lowering disagreement and
/// surface at seal.
pub(super) fn coerce_arm_value(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    value: ValueId,
    result_ty: &IRType,
) -> ValueId {
    if matches!(result_ty, IRType::Unit) && !matches!(ctx.type_of(value), IRType::Unit) {
        return emit_unit(ctx, block);
    }
    value
}

/// Map the typecheck-stamped result type on a control-flow
/// expression to its IR equivalent. Centralized so per-arm helpers
/// don't redo the registry walk.
pub(super) fn lower_result_ty(
    resolution: &ResolvedType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRType {
    resolved_type_to_ir_type(resolution, registry, &mut output.instantiations)
}

/// Emit a fresh `Const::Unit` in `block` and return its `ValueId`.
pub(super) fn emit_unit(ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::Unit);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::Unit,
        },
    );
    dest
}
