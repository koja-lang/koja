//! Statement-list driver. [`lower_body`] threads the open
//! [`IRBlockId`] through each [`Statement`] in order, returning the
//! trailing [`FlowResult`] so callers (`lower_function` /
//! `lower_arm_into`) can decide how to wire the block's terminator.
//!
//! [`lower_body_to_blocks`] is the script-mode seam: it owns its own
//! [`FnLowerCtx`], so [`crate::lower_script`] doesn't need to know
//! about the lowering context at all.
//!
//! The fail-fast contract for feature-gap diagnostics lives here:
//! the moment any helper returns `Err(())`, the surrounding function
//! is dropped (matched against per-function fail-fast) and the
//! diagnostic propagates back to `lower_program` /
//! `lower_script` via the shared `diagnostics` accumulator.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Diagnostic, Statement};

use crate::function::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::types::{IRType, ValueId};

use super::ctx::{FlowResult, FnLowerCtx};
use super::expr::lower_expr;

/// Lower a sequence of statements into a CFG fragment, starting in a
/// fresh `entry` block. Used by [`crate::lower_script`] to lower a
/// script body without exposing [`FnLowerCtx`] outside the
/// [`crate::lower`] module tree.
///
/// `Err(())` means "a feature-gap diagnostic was already pushed and
/// the caller should drop this body / function from the surrounding
/// fragment". This matches the per-function fail-fast policy
/// `lower_program` already implements; `lower_script` mirrors it for
/// the implicit script body.
pub(crate) fn lower_body_to_blocks(
    body: &[Statement],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(Vec<IRBasicBlock>, IRType), ()> {
    let mut ctx = FnLowerCtx::new();
    let entry = ctx.fresh_block("entry");
    let flow = lower_body(body, &mut ctx, entry, registry, diagnostics)?;
    let return_type = match &flow {
        FlowResult::Open {
            value: Some(id), ..
        } => ctx.type_of(*id),
        FlowResult::Open { value: None, .. } => IRType::Unit,
        // Closed-flow on a script body means an explicit `return`
        // exited the script. `Unit` is a defensible default here —
        // the auto-print wrapper inspects this type to pick a
        // printer, and a script that returns explicitly today only
        // does so via `return_value: Option<expr>` whose type the
        // body lowering already plumbed through `Return.value`.
        // Tightening this to "type of the returned value" is a
        // follow-up if/when scripts care.
        FlowResult::Closed => IRType::Unit,
    };
    finalize_open_flow(&mut ctx, flow);
    Ok((ctx.into_blocks(), return_type))
}

/// Walk a sequence of statements, threading the open block through
/// each one. Returns the trailing statement's flow result; an
/// empty body returns `Open { value: None, block: entry }`.
pub(super) fn lower_body(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    mut block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    let mut last_value: Option<ValueId> = None;
    for stmt in body {
        match lower_statement(stmt, ctx, block, registry, diagnostics)? {
            FlowResult::Open { value, block: next } => {
                last_value = value;
                block = next;
            }
            FlowResult::Closed => return Ok(FlowResult::Closed),
        }
    }
    Ok(FlowResult::Open {
        value: last_value,
        block,
    })
}

fn lower_statement(
    stmt: &Statement,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    match stmt {
        Statement::Expr(expr) => {
            let (value, next) = lower_expr(expr, ctx, block, registry, diagnostics)?;
            Ok(FlowResult::Open {
                value: Some(value),
                block: next,
            })
        }
        Statement::Return { value, .. } => {
            let return_value = match value.as_ref() {
                Some(expr) => {
                    let (id, next) = lower_expr(expr, ctx, block, registry, diagnostics)?;
                    ctx.cfg
                        .set_terminator(next, IRTerminator::Return { value: Some(id) });
                    Some(id)
                }
                None => {
                    ctx.cfg
                        .set_terminator(block, IRTerminator::Return { value: None });
                    None
                }
            };
            // Suppress the unused-binding warning while keeping the
            // shape parallel to the `if` / `unless` branches that
            // care about the returned value.
            let _ = return_value;
            Ok(FlowResult::Closed)
        }
        Statement::Assignment { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower `=` assignment statements",
                *span,
            ));
            Err(())
        }
        Statement::CompoundAssign { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower compound assignment statements",
                *span,
            ));
            Err(())
        }
        Statement::Break { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower `break` statements",
                *span,
            ));
            Err(())
        }
    }
}

/// Wire a still-open trailing flow up to its function's `Return`.
/// Closed flows already set their own terminator (an inner `return`);
/// nothing to do.
pub(super) fn finalize_open_flow(ctx: &mut FnLowerCtx, flow: FlowResult) {
    if let FlowResult::Open { value, block } = flow {
        ctx.cfg
            .set_terminator(block, IRTerminator::Return { value });
    }
}
