//! `match` expression lowering. Builds a chain of arm-test blocks
//! that fall through on miss, with each arm body branching into a
//! single merge block carrying the join value as a typed
//! [`crate::function::BlockParam`]. Same merge-block shape as
//! [`super::control_flow::lower_cond`]'s with-else path; the
//! distinguishing piece is the per-arm [`PatternCheck`] that gates
//! whether the arm body executes.
//!
//! Each arm's [`PatternCheck::Tests`] may carry multiple gating
//! predicates (single-test for `Literal` / `EnumUnit` /
//! `EnumTuple`, n-test for `Or`); the driver wires every step's
//! success edge to the arm's success block, every interior step's
//! failure edge to the next step, and the final step's failure
//! edge to either the next arm's first test block or — when the
//! arm chain has no successor — a synthesized `Unreachable` trap
//! block. Typecheck has already proven exhaustiveness for enum
//! subjects, so the trap is statically unreachable; it exists to
//! keep the CFG well-formed for backends that demand a terminator
//! on every block.
//!
//! When the arm carries a `when` guard, the success block is a
//! fresh `match_guard_<n>` that hosts the payload binds (so the
//! guard sees pattern-introduced locals) and ends in a
//! `CondBranch` on the guard's value, branching to the arm's body
//! on true and to the same fall-through on false. Without a guard,
//! the success block *is* the body block and binds happen at its
//! head as before.
//!
//! Block allocation is lazy: body / guard / next-test blocks are
//! minted only after the arm's [`PatternCheck`] is known. This way
//! arms following an unguarded catch-all (Phase 5 reachability
//! warns on them but typecheck still admits the source) are never
//! processed and contribute no orphan blocks to the CFG.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Expr, MatchArm};

use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::ownership::Ownership;
use crate::types::{IRType, ValueId};

use super::arms::lower_arm_into;
use super::ctx::{FnLowerCtx, LowerOutput, SlotStateSnapshot};
use super::expr::lower_expr;
use super::patterns::{
    BindOp, ChainMode, PatternCheck, PatternInputs, PayloadBind, TestStep, lower_pattern_check,
};

/// AST-side inputs to [`lower_match`]. Bundled per the same
/// `too_many_arguments` discipline [`super::control_flow::IfLowering`]
/// uses.
pub(super) struct MatchLowering<'a> {
    pub(super) subject: &'a Expr,
    pub(super) arms: &'a [MatchArm],
    pub(super) result_ty: IRType,
}

pub(super) fn lower_match(
    inputs: MatchLowering<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let MatchLowering {
        subject,
        arms,
        result_ty,
    } = inputs;
    let (subject_value, block) = lower_expr(subject, ctx, block, registry, output)?;

    let merge_block = ctx.fresh_block("match_merge");
    let result_id = ctx.declare_block_param(merge_block, result_ty.clone());

    let entry_snapshot = ctx.snapshot_slot_states();
    let mut arm_post_states: Vec<SlotStateSnapshot> = Vec::with_capacity(arms.len());

    let mut current_test = block;
    let mut closed_chain = false;
    let mut trap_block: Option<IRBlockId> = None;
    for (index, arm) in arms.iter().enumerate() {
        let body_block = ctx.fresh_block(format!("match_body_{index}"));
        let success_block = if arm.guard.is_some() {
            ctx.fresh_block(format!("match_guard_{index}"))
        } else {
            body_block
        };
        // Reset slot states to the construct-entry snapshot so the
        // arm body lowers from the runtime-accurate baseline (every
        // arm enters with the slot state that held before the
        // match). Without this, a prior arm's per-write ownership
        // stamp would bleed into this arm's reassignment-drop
        // emission and synthesize a `DropLocal` against runtime
        // values that the prior arm never produced.
        ctx.restore_slot_states(entry_snapshot.clone());
        let inputs = PatternInputs {
            registry,
            subject: subject_value,
            subject_ty: &subject.resolution,
        };
        let (check, _) = lower_pattern_check(&arm.pattern, inputs, ctx, current_test, output)?;
        // An unguarded catch-all has no failure edge, so it never
        // needs a fall-through. Any other arm — including a
        // guarded catch-all whose guard might be false — falls
        // through to the next arm's test block (or a shared trap
        // when this is the last arm).
        let needs_fall_through = match &check {
            PatternCheck::CatchAll { .. } => arm.guard.is_some(),
            PatternCheck::Tests { .. } => true,
        };
        let is_last = index + 1 == arms.len();
        let next_arm: Option<IRBlockId> = if needs_fall_through && !is_last {
            Some(ctx.fresh_block(format!("match_test_{}", index + 1)))
        } else {
            None
        };
        let fall_through = if needs_fall_through {
            next_arm.unwrap_or_else(|| trap_block_for(&mut trap_block, ctx))
        } else {
            success_block
        };
        match check {
            PatternCheck::CatchAll { binds } => {
                ctx.cfg.set_terminator(
                    current_test,
                    IRTerminator::Branch(BranchTarget::to(success_block)),
                );
                emit_payload_binds(&binds, success_block, subject_value, ctx);
                if arm.guard.is_none() {
                    closed_chain = true;
                }
            }
            PatternCheck::Tests {
                chain_mode,
                payload_binds,
                steps,
            } => {
                wire_test_chain(&steps, chain_mode, success_block, fall_through, ctx);
                emit_payload_binds(&payload_binds, success_block, subject_value, ctx);
            }
        }
        if let Some(guard) = &arm.guard {
            let (guard_value, after) = lower_expr(guard, ctx, success_block, registry, output)?;
            ctx.cfg.set_terminator(
                after,
                IRTerminator::CondBranch {
                    cond: guard_value,
                    else_target: BranchTarget::to(fall_through),
                    then_target: BranchTarget::to(body_block),
                },
            );
        }
        lower_arm_into(
            &arm.body,
            ctx,
            body_block,
            merge_block,
            &result_ty,
            registry,
            output,
        )?;
        arm_post_states.push(ctx.snapshot_slot_states());
        if let Some(next) = next_arm {
            current_test = next;
        }
        if closed_chain {
            break;
        }
    }

    // Join per-arm post-states into the merged post-match state.
    // Slots that every reachable arm agrees on keep their stamp;
    // disagreements fall back to `Unowned` (conservative — no
    // function-exit drop for a slot whose runtime ownership is
    // ambiguous). Empty `arm_post_states` falls back to the entry
    // snapshot, preserving the pre-match slot states untouched.
    if arm_post_states.is_empty() {
        ctx.restore_slot_states(entry_snapshot);
    } else {
        ctx.merge_slot_states(arm_post_states);
    }

    Ok((result_id, merge_block))
}

fn wire_test_chain(
    steps: &[TestStep],
    mode: ChainMode,
    success_block: IRBlockId,
    fall_through: IRBlockId,
    ctx: &mut FnLowerCtx,
) {
    for (index, step) in steps.iter().enumerate() {
        let next_step_block = steps.get(index + 1).map(|next| next.test_block);
        let (then_target, else_target) = match mode {
            ChainMode::And => (next_step_block.unwrap_or(success_block), fall_through),
            ChainMode::Or => (success_block, next_step_block.unwrap_or(fall_through)),
        };
        ctx.cfg.set_terminator(
            step.test_block,
            IRTerminator::CondBranch {
                cond: step.cond,
                else_target: BranchTarget::to(else_target),
                then_target: BranchTarget::to(then_target),
            },
        );
    }
}

fn emit_payload_binds(
    binds: &[PayloadBind],
    body_block: IRBlockId,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
) {
    for bind in binds {
        let mut current = subject;
        for step in &bind.chain {
            let dest = ctx.fresh_value(step.output_type.clone());
            match &step.op {
                BindOp::EnumPayloadField {
                    enum_symbol,
                    payload_index,
                    tag,
                } => {
                    ctx.cfg.append(
                        body_block,
                        IRInstruction::EnumPayloadFieldGet {
                            dest,
                            field_type: step.output_type.clone(),
                            payload_index: *payload_index,
                            tag: *tag,
                            ty: enum_symbol.clone(),
                            value: current,
                        },
                    );
                }
                BindOp::StructField {
                    field_index,
                    struct_symbol,
                } => {
                    ctx.cfg.append(
                        body_block,
                        IRInstruction::FieldGet {
                            base: current,
                            dest,
                            field_index: *field_index,
                            field_type: step.output_type.clone(),
                            struct_symbol: struct_symbol.clone(),
                        },
                    );
                }
                BindOp::UnionPayload {
                    member_index,
                    member_type,
                    union_type,
                } => {
                    ctx.cfg.append(
                        body_block,
                        IRInstruction::UnionPayloadGet {
                            dest,
                            member_index: *member_index,
                            member_type: member_type.clone(),
                            ty: union_type.clone(),
                            value: current,
                        },
                    );
                }
            }
            current = dest;
        }
        ctx.cfg.append(
            body_block,
            IRInstruction::LocalWrite {
                local: bind.local,
                ownership: Ownership::Unowned,
                value: current,
            },
        );
        ctx.mark_local_written(bind.local, Ownership::Unowned);
    }
}

/// Lazily mint a single `Unreachable`-terminated trap block shared
/// by every arm whose final-step failure edge has nowhere else to
/// go. Typecheck has proven these edges are statically unreachable;
/// the block keeps the CFG well-formed.
fn trap_block_for(slot: &mut Option<IRBlockId>, ctx: &mut FnLowerCtx) -> IRBlockId {
    if let Some(existing) = *slot {
        return existing;
    }
    let block = ctx.fresh_block("match_unreachable");
    ctx.cfg.set_terminator(block, IRTerminator::Unreachable);
    *slot = Some(block);
    block
}
