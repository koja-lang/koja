//! `if` / `unless` lowering. Both produce a 3-block CFG fragment
//! (cond → arm → merge); the only structural difference is which
//! arm of `CondBranch` the body block sits on. The merge block
//! always emits a `Const::Unit` placeholder as the conditional's
//! value — this slice ships unit-typed `if` / `unless` only.
//! Value-producing `if` / `else` (with `phi` nodes or memory slots)
//! lands with the locals slice.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Diagnostic, Expr, Statement};

use crate::function::{IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

use super::body::lower_body;
use super::ctx::{FlowResult, FnLowerCtx};
use super::expr::lower_expr;

/// Lower an `if cond do then_body end` (no-else). Adds a then-block
/// and a merge-block; terminates the current block with a
/// `CondBranch` to those; lowers the then-body inside the then-block
/// and falls through to merge unless the body closed flow with an
/// early `return`. Always produces a fresh `Const::Unit` in the
/// merge block as the if-expression's value. Caller is responsible
/// for rejecting `else_body.is_some()` upstream — this slice has no
/// path to lower it (value-producing `if` / `else` lands with the
/// locals slice).
pub(super) fn lower_if(
    condition: &Expr,
    then_body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, diagnostics)?;
    let then_block = ctx.fresh_block("if_then");
    let merge_block = ctx.fresh_block("if_merge");
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            then_block,
            else_block: merge_block,
        },
    );

    lower_arm_into(
        then_body,
        ctx,
        then_block,
        merge_block,
        registry,
        diagnostics,
    )?;

    Ok((emit_unit(ctx, merge_block), merge_block))
}

/// Lower an `unless cond do body end`. Identical wiring to `if` with
/// the arms swapped: cond=`true` skips to merge, cond=`false` runs
/// the body.
pub(super) fn lower_unless(
    condition: &Expr,
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, diagnostics)?;
    let body_block = ctx.fresh_block("unless_body");
    let merge_block = ctx.fresh_block("unless_merge");
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            then_block: merge_block,
            else_block: body_block,
        },
    );

    lower_arm_into(body, ctx, body_block, merge_block, registry, diagnostics)?;

    Ok((emit_unit(ctx, merge_block), merge_block))
}

/// Lower an arm of an `if` / `unless`: walk the body in `arm_block`,
/// then unconditionally jump to `merge_block` if the flow is still
/// open. Closed flow (early `return` inside the arm) leaves the
/// terminator already set; we don't overwrite it.
fn lower_arm_into(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    arm_block: IRBlockId,
    merge_block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ()> {
    match lower_body(body, ctx, arm_block, registry, diagnostics)? {
        FlowResult::Open { block, .. } => {
            ctx.cfg
                .set_terminator(block, IRTerminator::Branch(merge_block));
        }
        FlowResult::Closed => {}
    }
    Ok(())
}

/// Emit a fresh `Const::Unit` in `block` and return its `ValueId`.
fn emit_unit(ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
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
