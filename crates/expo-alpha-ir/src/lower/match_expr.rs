//! `match` expression lowering. Builds a linear chain of arm-test
//! blocks that fall through on miss, with each arm body branching
//! into a single merge block carrying the join value as a typed
//! [`crate::function::BlockParam`]. Same merge-block shape as
//! [`super::control_flow::lower_cond`]'s with-else path; the
//! distinguishing piece is the per-arm [`PatternCheck`] that gates
//! whether the arm body executes.
//!
//! Today's supported patterns are leaves (wildcard / binding /
//! literal), so every "test" is at most a single equality op + cond
//! branch; the catch-all arm closes the chain and uses an
//! unconditional branch to its body block.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Expr, MatchArm};

use crate::function::{BranchTarget, IRBlockId, IRTerminator};
use crate::types::{IRType, ValueId};

use super::arms::lower_arm_into;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::patterns::{PatternCheck, lower_pattern_check};

/// AST-side inputs to [`lower_match`]. Bundled per the same
/// `too_many_arguments` discipline [`super::control_flow::IfLowering`]
/// uses.
pub(super) struct MatchLowering<'a> {
    pub(super) subject: &'a Expr,
    pub(super) arms: &'a [MatchArm],
    pub(super) result_ty: IRType,
}

/// Lower a `match` expression. The subject is lowered into the
/// surrounding open-flow block; each arm's pattern emits its
/// gating instructions (literal compare or binding write) into the
/// "test block" for that arm, with cond=false falling through to
/// the next arm's test block. The catch-all arm short-circuits the
/// chain — its test block branches unconditionally into the arm
/// body. Every arm body branches into the merge block with its
/// tail value as the merge's per-edge `BranchTarget::args`.
///
/// Resolve guarantees a catch-all arm exists (the typecheck
/// exhaustiveness rule), so the chain is well-formed without an
/// explicit `unreachable` tail.
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

    let body_blocks: Vec<IRBlockId> = (0..arms.len())
        .map(|i| ctx.fresh_block(format!("match_body_{i}")))
        .collect();
    let test_blocks: Vec<IRBlockId> = (1..arms.len())
        .map(|i| ctx.fresh_block(format!("match_test_{i}")))
        .collect();

    let mut current_test = block;
    let mut closed_chain = false;
    for (index, arm) in arms.iter().enumerate() {
        let body_block = body_blocks[index];
        let next_test = test_blocks.get(index).copied();
        let (check, after_check) = lower_pattern_check(
            &arm.pattern,
            subject_value,
            &subject.resolution,
            ctx,
            current_test,
            registry,
            output,
        )?;
        match check {
            PatternCheck::CatchAll => {
                ctx.cfg.set_terminator(
                    after_check,
                    IRTerminator::Branch(BranchTarget::to(body_block)),
                );
                closed_chain = true;
            }
            PatternCheck::Predicate { cond } => {
                let fall_through = next_test.unwrap_or_else(|| {
                    // Resolve enforces a catch-all, so the last arm
                    // is always a CatchAll on the success path.
                    // Reaching this branch means the catch-all rule
                    // was bypassed — diagnostics already non-empty;
                    // fall through to the merge block with an
                    // arbitrary edge so the CFG stays well-formed.
                    body_blocks[index]
                });
                ctx.cfg.set_terminator(
                    after_check,
                    IRTerminator::CondBranch {
                        cond,
                        else_target: BranchTarget::to(fall_through),
                        then_target: BranchTarget::to(body_block),
                    },
                );
            }
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
        if let Some(next) = next_test {
            current_test = next;
        }
        if closed_chain {
            break;
        }
    }

    Ok((result_id, merge_block))
}
