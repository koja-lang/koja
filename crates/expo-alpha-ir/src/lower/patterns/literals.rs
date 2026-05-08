//! Literal-pattern lowering: emit `subject == const(value)` as a
//! single `Bool` value. Shared between the `Pattern::Literal` arm
//! of [`super::lower_pattern_check`] and the literal alternative
//! arm of [`super::or_pattern::emit_or_alternative`].

use expo_ast::ast::{Diagnostic, Literal};
use expo_ast::span::Span;

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::ops::{const_value_type, lower_literal};
use crate::function::{IRBlockId, IRInstruction};
use crate::types::{IRBinOp, IRType, ValueId};

/// Emit `subject == const(value)` into `block` and return the
/// resulting `Bool` value. Returns `Err` only when `lower_literal`
/// rejects the literal, which on the dispatcher path lets the
/// caller propagate. Or-pattern alternatives panic instead because
/// typecheck rejects non-lowerable literals before they reach this
/// path.
pub(super) fn emit_literal_eq(
    value: &Literal,
    span: Span,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ValueId, ()> {
    let const_value = lower_literal(value, span, diagnostics)?;
    let const_ty = const_value_type(&const_value);
    let const_dest = ctx.fresh_value(const_ty);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest: const_dest,
            value: const_value,
        },
    );
    let cond = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest: cond,
            lhs: subject,
            op: IRBinOp::Eq,
            rhs: const_dest,
        },
    );
    Ok(cond)
}

/// `emit_literal_eq` with `lower_literal` failures upgraded to a
/// panic. Used on the or-pattern path where typecheck has already
/// validated every alternative.
pub(super) fn emit_literal_eq_or_panic(
    value: &Literal,
    span: Span,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    emit_literal_eq(value, span, subject, ctx, block, &mut output.diagnostics)
        .expect("alpha IR lower: typecheck must have rejected non-lowerable literal")
}
