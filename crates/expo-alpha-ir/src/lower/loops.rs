//! `while` lowering. Builds a header / body / exit CFG fragment
//! whose shape mirrors v1's [`expo-ir::lower::loops`]:
//!
//! ```text
//! open ─Branch─▶ header ─CondBranch─┬─▶ body ─Branch (back-edge)─▶ header
//!                                    └─▶ exit (continue lowering)
//! ```
//!
//! Loop-carried state lives in alloca slots, not block params: alpha's
//! mutable bindings already model state through
//! [`crate::IRInstruction::LocalDecl`] + [`crate::IRInstruction::LocalWrite`]
//! against per-slot allocas, and each iteration's body re-reads /
//! writes the slot directly. Block-param SSA stays for `if`/`else`
//! arm joins inside the loop body, unchanged.
//!
//! The surface expression is `Unit`-typed (mirrors v1's
//! `infer_expr` for `While`); the produced `ValueId` is a fresh
//! `Const::Unit` emitted at the top of the exit block so callers can
//! continue threading values through the open flow.

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

    match lower_body(body, ctx, body_block, registry, output)? {
        FlowResult::Open { block: tail, .. } => {
            ctx.cfg
                .set_terminator(tail, IRTerminator::Branch(BranchTarget::to(header)));
        }
        // Body closed its own flow (early `return`); no back-edge to
        // emit. The header's CondBranch to `body_block` still names a
        // valid block — its terminator was set inside `lower_body`.
        FlowResult::Closed => {}
    }

    let unit = emit_unit(ctx, exit_block);
    Ok((unit, exit_block))
}
