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
use expo_ast::ast::{AssignTarget, CompoundOp, Diagnostic, Expr, LValue, Statement};
use expo_ast::identifier::LocalId;

use crate::function::{IRBasicBlock, IRBlockId, IRInstruction, IRTerminator};
use crate::local::IRLocalId;
use crate::types::{IRBinOp, IRType, ValueId};

use super::ctx::{FlowResult, FnLowerCtx};
use super::expr::lower_expr;
use super::ops::bin_op_result_type;

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
        Statement::Assignment { target, value, .. } => {
            lower_assignment(target, value, ctx, block, registry, diagnostics)
        }
        Statement::CompoundAssign {
            target, op, value, ..
        } => lower_compound_assignment(target, *op, value, ctx, block, registry, diagnostics),
        Statement::Break { span } => {
            diagnostics.push(Diagnostic::error(
                "alpha IR does not yet lower `break` statements",
                *span,
            ));
            Err(())
        }
    }
}

/// Lower a `Statement::Assignment` to (optional) `LocalDecl` + `LocalWrite`.
/// Typecheck-resolve has already stamped the target Ident with
/// [`Resolution::Local`] (carrying the AST [`LocalId`]) and rejected
/// every shape that doesn't fit a single-segment local name, so this
/// helper assumes the well-typed shape and panics on deviation.
///
/// First write of a local emits a `LocalDecl` into the function's
/// entry block (regardless of which block the assignment statement
/// surface-syntactically lives in) so backends see a single decl per
/// slot at the canonical entry-block position. Subsequent writes
/// just emit the `LocalWrite` in the currently-open block.
///
/// Returns `Open { value: None, ... }` because assignment is
/// statement-level vocabulary — its trailing value is the rhs's
/// [`ValueId`], but no surface syntax in this slice consumes it
/// directly. (Trailing-expression-of-body checking runs on the
/// trailing `Statement::Expr`, not on assignments.)
///
/// [`LocalId`]: expo_ast::identifier::LocalId
fn lower_assignment(
    target: &AssignTarget,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    let local_id = single_segment_local(target);
    let ir_local = IRLocalId::from_local_id(local_id);

    let (value_id, current) = lower_expr(value, ctx, block, registry, diagnostics)?;
    let value_ty = ctx.type_of(value_id);

    if !ctx.local_is_declared(ir_local) {
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: value_ty,
            },
        );
        ctx.mark_local_declared(ir_local);
    }
    ctx.cfg.append(
        current,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: value_id,
        },
    );
    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

/// Lower `target op= value` to `LocalRead + BinaryOp + LocalWrite`.
/// Typecheck-resolve guarantees the local was already declared with
/// an arithmetic type and that the rhs's type matches, so this
/// helper assumes a well-typed shape and panics on deviation. Unlike
/// [`lower_assignment`], we never emit a `LocalDecl` — compound
/// assignment is reassignment-only.
fn lower_compound_assignment(
    target: &LValue,
    op: CompoundOp,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<FlowResult, ()> {
    let local_id = single_segment_lvalue(target);
    let ir_local = IRLocalId::from_local_id(local_id);

    let (rhs, current) = lower_expr(value, ctx, block, registry, diagnostics)?;
    let ty = ctx.type_of(rhs);

    let read_dest = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        current,
        IRInstruction::LocalRead {
            dest: read_dest,
            local: ir_local,
            ty: ty.clone(),
        },
    );

    let ir_op = compound_to_ir(op);
    let result = ctx.fresh_value(bin_op_result_type(ir_op, ty));
    ctx.cfg.append(
        current,
        IRInstruction::BinaryOp {
            dest: result,
            lhs: read_dest,
            op: ir_op,
            rhs,
        },
    );
    ctx.cfg.append(
        current,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: result,
        },
    );

    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

fn compound_to_ir(op: CompoundOp) -> IRBinOp {
    match op {
        CompoundOp::Add => IRBinOp::Add,
        CompoundOp::Div => IRBinOp::Div,
        CompoundOp::Mul => IRBinOp::Mul,
        CompoundOp::Sub => IRBinOp::Sub,
    }
}

/// Pull the [`LocalId`] off a sealed assignment target. Typecheck
/// rejects pattern destructuring, so by the time this runs the
/// target is an [`AssignTarget::LValue`] whose [`LValue`] passes
/// `single_segment_lvalue`'s checks.
fn single_segment_local(target: &AssignTarget) -> LocalId {
    let AssignTarget::LValue(lvalue) = target else {
        panic!(
            "alpha IR lower: assignment target must be an LValue after typecheck seal \
             (got {target:?})",
        );
    };
    single_segment_lvalue(lvalue)
}

/// Validate a sealed compound-assign / regular-assign LValue and
/// return its [`LocalId`]. Single-segment shape and a stamped
/// `local_id` are typecheck-seal invariants; deviation is an
/// upstream bug.
fn single_segment_lvalue(lvalue: &LValue) -> LocalId {
    if lvalue.segments.len() != 1 {
        panic!(
            "alpha IR lower: assignment target must be single-segment after typecheck seal \
             (got {} segments)",
            lvalue.segments.len(),
        );
    }
    lvalue.local_id.unwrap_or_else(|| {
        panic!(
            "alpha IR lower: single-segment assignment target `{}` carries no LocalId — \
             typecheck resolve invariant violation",
            lvalue.segments[0],
        )
    })
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
