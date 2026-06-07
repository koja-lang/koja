//! `if` / `unless` / `cond` lowering. Each builds a CFG fragment
//! whose merge block carries the join value as a typed
//! [`BlockParam`]; reaching arms branch into the merge with the
//! arm's tail value as the per-edge [`BranchTarget::args`] payload.
//! Diverging arms (early `return`) close their own flow and don't
//! contribute an edge to the merge — the seal pass admits a merge
//! block with fewer incomings than predecessors.
//!
//! The "no-else `if` / `unless` are statement-shaped" path stays
//! Unit-typed: the merge block's [`BlockParam`] is `Unit` and the
//! cond=false / cond=true edge that bypasses the body passes a
//! freshly-emitted `Const::Unit` so every edge carries a
//! type-matching arg.

use koja_ast::ast::{CondArm, Expr, Statement};
use koja_typecheck::GlobalRegistry;

use crate::function::{BranchTarget, IRBlockId, IRTerminator};
use crate::types::{IRType, ValueId};

use super::arms::{emit_unit, lower_arm_into, lower_expr_arm_into};
use super::ctx::{FnLowerCtx, LowerOutput, SlotStateSnapshot};
use super::expr::lower_expr;

/// AST-side inputs to [`lower_if`]. Bundled so the helper signature
/// stays under the clippy `too_many_arguments` threshold without
/// losing per-field readability at the dispatch site (which builds
/// one of these inline from `ExprKind::If`'s own fields).
pub(super) struct IfLowering<'a> {
    pub(super) condition: &'a Expr,
    pub(super) else_body: Option<&'a [Statement]>,
    pub(super) result_ty: IRType,
    pub(super) then_body: &'a [Statement],
}

/// Lower an `if cond do then_body else else_body end`. The merge
/// block declares one [`BlockParam`] typed by `result_ty`, every
/// reaching arm hands its tail value to the merge as a per-edge
/// branch arg, and the surface expression's value is the merge
/// param's `ValueId`.
///
/// `result_ty` is sourced from the typecheck-stamped resolution on
/// the surrounding `if`-expression — when both arms diverge the
/// resolution is `Never` (mapped to `IRType::Unit` at the IR
/// boundary in [`super::package::resolved_type_to_ir_type`]); the
/// merge block in that case is unreachable but still synthesized so
/// any surrounding control flow continues to have a continuation
/// block to thread through.
///
/// No-`else` (`else_body == None`) keeps the pre-block-params
/// statement shape: the cond=false edge bypasses any arm body and
/// passes a synthesized `Const::Unit` to the merge directly.
pub(super) fn lower_if(
    inputs: IfLowering<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let IfLowering {
        condition,
        else_body,
        result_ty,
        then_body,
    } = inputs;
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, output)?;
    let then_block = ctx.fresh_block("if_then");
    let merge_block = ctx.fresh_block("if_merge");
    let result_id = ctx.declare_merge_param(merge_block, result_ty.clone());

    let (else_target, else_block) = match else_body {
        Some(_) => {
            let id = ctx.fresh_block("if_else");
            (BranchTarget::to(id), Some(id))
        }
        None => {
            let unit = emit_unit(ctx, block);
            (BranchTarget::with_args(merge_block, vec![unit]), None)
        }
    };
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            else_target,
            then_target: BranchTarget::to(then_block),
        },
    );

    let entry_snapshot = ctx.snapshot_slot_states();
    let mut post_states: Vec<SlotStateSnapshot> = Vec::with_capacity(2);

    lower_arm_into(
        then_body,
        ctx,
        then_block,
        merge_block,
        &result_ty,
        registry,
        output,
    )?;
    post_states.push(ctx.snapshot_slot_states());

    if let (Some(else_body), Some(else_block)) = (else_body, else_block) {
        ctx.restore_slot_states(entry_snapshot.clone());
        lower_arm_into(
            else_body,
            ctx,
            else_block,
            merge_block,
            &result_ty,
            registry,
            output,
        )?;
        post_states.push(ctx.snapshot_slot_states());
    } else {
        // No `else` arm: the cond=false edge bypasses the body
        // straight to the merge, so the "else" post-state is the
        // entry snapshot (no slot writes occur on that path).
        post_states.push(entry_snapshot);
    }
    ctx.merge_slot_states(post_states);
    Ok((result_id, merge_block))
}

/// Lower an `unless cond do body end`. Same wiring as `lower_if`'s
/// no-else path with the cond arms swapped: cond=true bypasses to
/// merge with `Unit`, cond=false runs the body. Statement-shaped
/// only — `unless` has no `else` arm, so the result type is always
/// `Unit`.
pub(super) fn lower_unless(
    condition: &Expr,
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, output)?;
    let body_block = ctx.fresh_block("unless_body");
    let merge_block = ctx.fresh_block("unless_merge");
    let result_id = ctx.declare_merge_param(merge_block, IRType::Unit);

    let bypass_unit = emit_unit(ctx, block);
    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            else_target: BranchTarget::to(body_block),
            then_target: BranchTarget::with_args(merge_block, vec![bypass_unit]),
        },
    );
    let entry_snapshot = ctx.snapshot_slot_states();
    lower_arm_into(
        body,
        ctx,
        body_block,
        merge_block,
        &IRType::Unit,
        registry,
        output,
    )?;
    let body_post = ctx.snapshot_slot_states();
    // Merge the body-arm post-state with the bypass-arm post-state
    // (= entry snapshot, since the cond=true path skips the body
    // and writes no slots).
    ctx.merge_slot_states(vec![body_post, entry_snapshot]);
    Ok((result_id, merge_block))
}

/// AST-side inputs to [`lower_cond`]. See [`IfLowering`] for the
/// motivation.
pub(super) struct CondLowering<'a> {
    pub(super) arms: &'a [CondArm],
    pub(super) else_body: Option<&'a [Statement]>,
    pub(super) result_ty: IRType,
}

/// Lower a `cond a do … b do … else … end` chain. Same merge-block
/// shape as `lower_if`'s with-else path: one [`BlockParam`] typed
/// by `result_ty`, every reaching arm body branches to merge with
/// its tail value, the else-body covers the "no arm matched" exit.
///
/// The arm test chain is built as a sequence of `cond_test_<i>`
/// blocks that fall through to the next test on cond=false; the
/// final test's cond=false edge runs the else-body. Each arm body
/// lives in its own `cond_body_<i>` block.
pub(super) fn lower_cond(
    inputs: CondLowering<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let CondLowering {
        arms,
        else_body,
        result_ty,
    } = inputs;
    let merge_block = ctx.fresh_block("cond_merge");
    let result_id = ctx.declare_merge_param(merge_block, result_ty.clone());

    // Pre-allocate one body-block per arm and one chained test-block
    // per non-first arm. The first arm's test runs in the surrounding
    // `block` (the cond expression's open-flow continuation), so we
    // don't pre-allocate one for it.
    let body_blocks: Vec<IRBlockId> = (0..arms.len())
        .map(|i| ctx.fresh_block(format!("cond_body_{i}")))
        .collect();
    let chained_test_blocks: Vec<IRBlockId> = (1..arms.len())
        .map(|i| ctx.fresh_block(format!("cond_test_{i}")))
        .collect();
    let else_block = else_body.map(|_| ctx.fresh_block("cond_else"));

    let entry_snapshot = ctx.snapshot_slot_states();
    let arm_count = arms.len() + usize::from(else_body.is_some());
    let mut post_states: Vec<SlotStateSnapshot> = Vec::with_capacity(arm_count);

    let mut current_test = block;
    for (index, arm) in arms.iter().enumerate() {
        let (cond_value, after_cond) =
            lower_expr(&arm.condition, ctx, current_test, registry, output)?;
        let body_block = body_blocks[index];
        let next_test = chained_test_blocks.get(index).copied();
        let fall_through = match (next_test, else_block) {
            (Some(next), _) => BranchTarget::to(next),
            (None, Some(else_block)) => BranchTarget::to(else_block),
            (None, None) => {
                // No else and we exhausted arm tests: cond=false on
                // the last arm flows directly to merge with `Unit`.
                // (The parser always produces an else; this path is
                // defensive parity with `resolve_cond`.)
                let unit = emit_unit(ctx, after_cond);
                BranchTarget::with_args(merge_block, vec![unit])
            }
        };
        ctx.cfg.set_terminator(
            after_cond,
            IRTerminator::CondBranch {
                cond: cond_value,
                else_target: fall_through,
                then_target: BranchTarget::to(body_block),
            },
        );
        ctx.restore_slot_states(entry_snapshot.clone());
        lower_arm_into(
            &arm.body,
            ctx,
            body_block,
            merge_block,
            &result_ty,
            registry,
            output,
        )?;
        post_states.push(ctx.snapshot_slot_states());
        if let Some(next) = next_test {
            current_test = next;
        }
    }

    if let (Some(else_body), Some(else_block)) = (else_body, else_block) {
        ctx.restore_slot_states(entry_snapshot.clone());
        lower_arm_into(
            else_body,
            ctx,
            else_block,
            merge_block,
            &result_ty,
            registry,
            output,
        )?;
        post_states.push(ctx.snapshot_slot_states());
    } else if !arms.is_empty() {
        // No else and parser-produced cond: contribute the
        // entry-snapshot to the merge so a slot that some arm
        // writes (and others don't) doesn't get over-promoted.
        post_states.push(entry_snapshot.clone());
    }
    if post_states.is_empty() {
        ctx.restore_slot_states(entry_snapshot);
    } else {
        ctx.merge_slot_states(post_states);
    }
    Ok((result_id, merge_block))
}

/// AST-side inputs to [`lower_ternary`]. See [`IfLowering`] for the
/// motivation. Ternary arms are expressions rather than statement
/// bodies, so the carried payload is one `Expr` per arm rather than
/// a `&[Statement]`, and we always have both arms (the parser
/// requires `else_expr`).
pub(super) struct TernaryLowering<'a> {
    pub(super) condition: &'a Expr,
    pub(super) then_expr: &'a Expr,
    pub(super) else_expr: &'a Expr,
    pub(super) result_ty: IRType,
}

/// Lower a `cond ? then_expr : else_expr` ternary. Same merge-block
/// shape as `lower_if`'s with-else path: one [`BlockParam`] typed
/// by `result_ty`, each arm branches into the merge with the arm's
/// expression value as the per-edge branch arg. Strictly simpler
/// than `lower_if` because the arms are single expressions — no
/// statement-body walk, no [`FlowResult::Closed`] bookkeeping (a
/// ternary arm cannot syntactically contain a `return`).
pub(super) fn lower_ternary(
    inputs: TernaryLowering<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let TernaryLowering {
        condition,
        then_expr,
        else_expr,
        result_ty,
    } = inputs;
    let (cond_value, block) = lower_expr(condition, ctx, block, registry, output)?;
    let then_block = ctx.fresh_block("ternary_then");
    let else_block = ctx.fresh_block("ternary_else");
    let merge_block = ctx.fresh_block("ternary_merge");
    let result_id = ctx.declare_merge_param(merge_block, result_ty.clone());

    ctx.cfg.set_terminator(
        block,
        IRTerminator::CondBranch {
            cond: cond_value,
            else_target: BranchTarget::to(else_block),
            then_target: BranchTarget::to(then_block),
        },
    );

    let entry_snapshot = ctx.snapshot_slot_states();

    lower_expr_arm_into(
        then_expr,
        ctx,
        then_block,
        merge_block,
        &result_ty,
        registry,
        output,
    )?;
    let then_post = ctx.snapshot_slot_states();
    ctx.restore_slot_states(entry_snapshot);
    lower_expr_arm_into(
        else_expr,
        ctx,
        else_block,
        merge_block,
        &result_ty,
        registry,
        output,
    )?;
    let else_post = ctx.snapshot_slot_states();
    ctx.merge_slot_states(vec![then_post, else_post]);
    Ok((result_id, merge_block))
}
