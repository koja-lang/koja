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
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::patterns::{
    BindSource, PatternCheck, PatternInputs, PayloadBind, TestStep, lower_pattern_check,
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
                payload_binds,
                steps,
            } => {
                wire_test_chain(&steps, success_block, fall_through, ctx);
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
        if let Some(next) = next_arm {
            current_test = next;
        }
        if closed_chain {
            break;
        }
    }

    Ok((result_id, merge_block))
}

fn wire_test_chain(
    steps: &[TestStep],
    body_block: IRBlockId,
    fall_through: IRBlockId,
    ctx: &mut FnLowerCtx,
) {
    for (index, step) in steps.iter().enumerate() {
        let else_block = steps
            .get(index + 1)
            .map(|next| next.test_block)
            .unwrap_or(fall_through);
        ctx.cfg.set_terminator(
            step.test_block,
            IRTerminator::CondBranch {
                cond: step.cond,
                else_target: BranchTarget::to(else_block),
                then_target: BranchTarget::to(body_block),
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
        let dest = ctx.fresh_value(bind.field_type.clone());
        match &bind.source {
            BindSource::EnumPayload {
                enum_symbol,
                payload_index,
                tag,
            } => {
                ctx.cfg.append(
                    body_block,
                    IRInstruction::EnumPayloadFieldGet {
                        dest,
                        field_type: bind.field_type.clone(),
                        payload_index: *payload_index,
                        tag: *tag,
                        ty: enum_symbol.clone(),
                        value: subject,
                    },
                );
            }
            BindSource::StructField {
                field_index,
                struct_symbol,
            } => {
                ctx.cfg.append(
                    body_block,
                    IRInstruction::FieldGet {
                        base: subject,
                        dest,
                        field_index: *field_index,
                        field_type: bind.field_type.clone(),
                        struct_symbol: struct_symbol.clone(),
                    },
                );
            }
        }
        ctx.cfg.append(
            body_block,
            IRInstruction::LocalWrite {
                local: bind.local,
                ownership: Ownership::Unowned,
                value: dest,
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
