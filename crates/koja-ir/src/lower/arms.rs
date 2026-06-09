//! Shared arm-lowering helpers used by `if` / `cond` / `unless` /
//! ternary / `match`. Each construct synthesizes a merge block with
//! one [`crate::function::BlockParam`] per join and routes its
//! reaching arms here so coercion + the fall-through `Branch`
//! terminator land in one place.
//!
//! Pulled out of [`super::control_flow`] so [`super::match_expr`]
//! can reuse the same exit pattern without re-importing private
//! helpers.

use koja_ast::ast::{Expr, Statement};
use koja_ast::identifier::ResolvedType;
use koja_typecheck::GlobalRegistry;

use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

use super::body::lower_body;
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::ownership::{drop_discarded_temp, materialize_owned};
use super::package::resolved_type_to_ir_type;

/// Lower a statement-body arm into `arm_block`, then unconditionally
/// jump to `merge_block` when flow stays open. Closed flow (early
/// `return`) leaves the existing terminator in place; the merge
/// block tolerates one fewer incoming edge.
///
/// Returns the arm's open tail block (where the merge branch landed)
/// so callers can append end-of-arm cleanup — e.g. `match` releasing
/// its consumed subject temp — after the tail value has been
/// acquired. `None` when the arm closed flow with an early `return`.
pub(super) fn lower_arm_into(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    arm_block: IRBlockId,
    merge_block: IRBlockId,
    result_ty: &IRType,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<Option<IRBlockId>, ()> {
    match lower_body(body, ctx, arm_block, registry, output)? {
        FlowResult::Open { block, value } => {
            let arg = match value {
                Some(id) => finalize_arm_value(ctx, block, id, result_ty),
                None => emit_unit(ctx, block),
            };
            ctx.cfg.set_terminator(
                block,
                IRTerminator::Branch(BranchTarget::with_args(merge_block, vec![arg])),
            );
            Ok(Some(block))
        }
        FlowResult::Closed => Ok(None),
    }
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
    let arg = finalize_arm_value(ctx, after, value, result_ty);
    ctx.cfg.set_terminator(
        after,
        IRTerminator::Branch(BranchTarget::with_args(merge_block, vec![arg])),
    );
    Ok(())
}

/// Conform an arm tail value to the merge block's `BlockParam` type
/// and hand the merge an owned value it can release.
///
/// - `result_ty == Unit` and the arm produced something else: the
///   tail value is discarded (a no-else `if`'s then-arm tails on a
///   non-Unit value the surrounding expression types as `Unit`), so
///   free it if it owns a heap temp and substitute a fresh
///   `Const::Unit` to keep the merge edge type-consistent.
/// - Otherwise (the arm produced a value of `result_ty`): *acquire*
///   it so the merge `BlockParam` — which the construct's lowering
///   marks `owned` for heap-managed results — owns an independent
///   reference. An owned tail moves; a borrowed one clones.
///
/// Other mismatches indicate a typecheck/lowering disagreement and
/// surface at seal.
pub(super) fn finalize_arm_value(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    value: ValueId,
    result_ty: &IRType,
) -> ValueId {
    if matches!(result_ty, IRType::Unit) && !matches!(ctx.type_of(value), IRType::Unit) {
        drop_discarded_temp(ctx, block, value);
        return emit_unit(ctx, block);
    }
    let ty = ctx.type_of(value);
    materialize_owned(ctx, block, value, &ty)
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
