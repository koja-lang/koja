//! `loop` and `while` lowering. Both build a body-and-exit CFG
//! fragment whose shape mirrors v1's [`koja-ir::lower::loops`]:
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
//! Loop-carried state lives in alloca slots, not block params: the pipeline's
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
//! `break` is lowered in [`super::body::lower_break_stmt`].
//! `lower_loop` only manages the [`FnLowerCtx::push_loop_exit`] /
//! [`FnLowerCtx::pop_loop_exit`] bookkeeping that gives `break` an
//! exit block to target.

use koja_ast::ast::{Expr, Statement};
use koja_typecheck::GlobalRegistry;

use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::types::ValueId;

use super::arms::emit_unit;
use super::body::lower_body;
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput, SlotStateSnapshot};
use super::expr::lower_expr;

/// Lower a `while cond ... end`. Builds three blocks:
///
/// - `header`: lowers `cond` and terminates with
///   [`IRTerminator::CondBranch`] to body or exit.
/// - `body`: lowers the body statements. The trailing flow's
///   terminator is the back-edge [`IRTerminator::Branch`] to the
///   header. A body that closes its own flow (an early `return`)
///   leaves no back-edge, and the merge block tolerates the missing
///   incoming.
/// - `exit`: receives the cond=false fall-through. The surface
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
    let body_snapshot = ctx.snapshot_slot_states();
    let body_flow = lower_body(body, ctx, body_block, registry, output)?;
    match body_flow {
        FlowResult::Open { block: tail, .. } => {
            drop_body_scoped_bindings(ctx, tail, &body_snapshot);
            ctx.cfg
                .set_terminator(tail, IRTerminator::Branch(BranchTarget::to(header)));
        }
        // Body closed its own flow (early `return` / `break`), so
        // there is no back-edge to emit. The header's CondBranch to
        // `body_block` still names a valid block. Its terminator was
        // set inside `lower_body`.
        FlowResult::Closed => {}
    }
    ctx.restore_slot_states(body_snapshot);
    ctx.pop_loop_exit();

    let unit = emit_unit(ctx, exit_block);
    Ok((unit, exit_block))
}

/// Release every heap-managed binding declared inside a loop body at
/// the back-edge `tail`. A binding introduced in the body leaves
/// scope when the iteration ends, so its owned value is dropped here.
/// Each iteration overwrites a fresh value and drops it before the
/// next, with no stale-overwrite drop on the slot (the body's single
/// lowered `LocalWrite` was a first declaration). Crucially, these
/// slots are then restored out of the live set by the caller so they
/// never reach the function-exit drops, where a zero-trip loop would
/// leave them uninitialized.
fn drop_body_scoped_bindings(
    ctx: &mut FnLowerCtx,
    tail: IRBlockId,
    body_snapshot: &SlotStateSnapshot,
) {
    for (local, ty) in ctx.heap_slots_declared_since(body_snapshot) {
        ctx.cfg.append(tail, IRInstruction::DropLocal { local, ty });
    }
}

/// Lower an infinite `loop ... end`. Builds two blocks (no header,
/// since there's no condition):
///
/// - `body`: lowers the body statements. The trailing flow's
///   terminator is the back-edge [`IRTerminator::Branch`] to itself.
///   A body that closes its own flow (an early `return` or `break`)
///   leaves no back-edge.
/// - `exit`: only reachable via [`super::body::lower_break_stmt`].
///   Produces a fresh `Const::Unit` so the caller can keep threading.
///   When the body has no `break`, the exit block stays unreachable.
///   That's intentional and harmless (every emitted block carries
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
    let body_snapshot = ctx.snapshot_slot_states();
    let body_flow = lower_body(body, ctx, body_block, registry, output)?;
    if let FlowResult::Open { block: tail, .. } = body_flow {
        drop_body_scoped_bindings(ctx, tail, &body_snapshot);
        ctx.cfg
            .set_terminator(tail, IRTerminator::Branch(BranchTarget::to(body_block)));
    }
    ctx.restore_slot_states(body_snapshot);
    ctx.pop_loop_exit();

    let unit = emit_unit(ctx, exit_block);
    Ok((unit, exit_block))
}
