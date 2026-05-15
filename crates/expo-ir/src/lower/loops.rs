//! `loop` and `while` lowering. Both build a body-and-exit CFG
//! fragment whose shape mirrors v1's [`expo-ir::lower::loops`]:
//!
//! ```text
//! while:
//!   open ─Branch─▶ header ─CondBranch─┬─▶ body ─Branch (back-edge)─▶ header
//!                                      └─▶ exit (continue lowering)
//! loop:
//!   open ─Branch─▶ body ─Branch (back-edge)─▶ body
//!                       └─Branch (break)──▶ exit (continue lowering)
//! ```
//!
//! Loop-carried state lives in alloca slots, not block params: alpha's
//! mutable bindings already model state through
//! [`crate::IRInstruction::LocalDecl`] + [`crate::IRInstruction::LocalWrite`]
//! against per-slot allocas, and each iteration's body re-reads /
//! writes the slot directly. Block-param SSA stays for `if`/`else`
//! arm joins inside the loop body, unchanged.
//!
//! Both shapes produce a `Unit` `ValueId` from the exit block so
//! callers can continue threading values through the open flow. The
//! surface type may be `Never` (an unbroken `loop`), but
//! [`super::package::resolved_type_to_ir_type`] maps `Never -> Unit`,
//! so the IR-level value is always concrete `Unit`.
//!
//! `break` is lowered in [`super::body::lower_break_stmt`] —
//! `lower_loop` only manages the [`FnLowerCtx::push_loop_exit`] /
//! [`FnLowerCtx::pop_loop_exit`] bookkeeping that gives `break` an
//! exit block to target.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Expr, Statement};

use crate::function::{BranchTarget, IRBlockId, IRTerminator};
use crate::types::ValueId;

use super::arms::emit_unit;
use super::body::lower_body;
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::expr::lower_expr;

/// Lower a `while cond ... end`. Builds three blocks:
///
/// - `header`: lowers `cond` and terminates with
///   [`IRTerminator::CondBranch`] to body or exit.
/// - `body`: lowers the body statements; the trailing flow's
///   terminator is the back-edge [`IRTerminator::Branch`] to the
///   header. A body that closes its own flow (an early `return`)
///   leaves no back-edge — the merge block tolerates the missing
///   incoming.
/// - `exit`: receives the cond=false fall-through; the surface
///   expression's [`ValueId`] is a fresh `Const::Unit` emitted here
///   so the caller can keep threading.
pub(super) fn lower_while(
    condition: &Expr,
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let header = ctx.fresh_block("while_header");
    let body_block = ctx.fresh_block("while_body");
    let exit_block = ctx.fresh_block("while_exit");

    ctx.cfg
        .set_terminator(block, IRTerminator::Branch(BranchTarget::to(header)));

    let (cond_value, header_tail) = lower_expr(condition, ctx, header, registry, output)?;
    ctx.cfg.set_terminator(
        header_tail,
        IRTerminator::CondBranch {
            cond: cond_value,
            else_target: BranchTarget::to(exit_block),
            then_target: BranchTarget::to(body_block),
        },
    );

    ctx.push_loop_exit(exit_block);
    let body_flow = lower_body(body, ctx, body_block, registry, output)?;
    ctx.pop_loop_exit();
    match body_flow {
        FlowResult::Open { block: tail, .. } => {
            ctx.cfg
                .set_terminator(tail, IRTerminator::Branch(BranchTarget::to(header)));
        }
        // Body closed its own flow (early `return` / `break`); no
        // back-edge to emit. The header's CondBranch to `body_block`
        // still names a valid block — its terminator was set inside
        // `lower_body`.
        FlowResult::Closed => {}
    }

    let unit = emit_unit(ctx, exit_block);
    Ok((unit, exit_block))
}

/// Lower an infinite `loop ... end`. Builds two blocks (no header —
/// there's no condition):
///
/// - `body`: lowers the body statements; the trailing flow's
///   terminator is the back-edge [`IRTerminator::Branch`] to itself.
///   A body that closes its own flow (an early `return` or `break`)
///   leaves no back-edge.
/// - `exit`: only reachable via [`super::body::lower_break_stmt`];
///   produces a fresh `Const::Unit` so the caller can keep threading.
///   When the body has no `break`, the exit block stays unreachable
///   — that's intentional and harmless (every emitted block carries
///   its own terminator regardless of reachability).
pub(super) fn lower_loop(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let body_block = ctx.fresh_block("loop_body");
    let exit_block = ctx.fresh_block("loop_exit");

    ctx.cfg
        .set_terminator(block, IRTerminator::Branch(BranchTarget::to(body_block)));

    ctx.push_loop_exit(exit_block);
    let body_flow = lower_body(body, ctx, body_block, registry, output)?;
    ctx.pop_loop_exit();
    if let FlowResult::Open { block: tail, .. } = body_flow {
        ctx.cfg
            .set_terminator(tail, IRTerminator::Branch(BranchTarget::to(body_block)));
    }

    let unit = emit_unit(ctx, exit_block);
    Ok((unit, exit_block))
}
