//! Literal-pattern lowering: emit `subject == const(value)` as a
//! single `Bool` value. Shared between the `Pattern::Literal` arm
//! of [`super::lower_pattern_check`] and the literal alternative
//! arm of [`super::or_pattern::emit_or_alternative`].

use koja_ast::ast::{Diagnostic, Literal};
use koja_ast::span::Span;
use koja_typecheck::LiteralCoercion;

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::super::ops::{const_value_type, lower_literal};
use crate::function::{IRBlockId, IRInstruction};
use crate::types::{IRBinOp, IRType, ValueId};

/// Emit `subject == const(value)` into `block` and return the
/// resulting `Bool` value. The pattern's stamped
/// `literal_coercion` (when present) drives the const's width so
/// the equality compares at the subject's runtime type — e.g.
/// `match x: UInt8 -> 5 -> ...` mints `Const u8 = 5` rather than
/// the default `Const i64`. Returns `Err` only when `lower_literal`
/// rejects the literal, which on the dispatcher path lets the
/// caller propagate. Or-pattern alternatives panic instead because
/// typecheck rejects non-lowerable literals before they reach this
/// path.
pub(super) fn emit_literal_eq(
    value: &Literal,
    coercion: Option<&LiteralCoercion>,
    span: Span,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ValueId, ()> {
    let target = coercion.and_then(LiteralCoercion::numeric_width);
    let const_value = lower_literal(value, span, target, diagnostics)?;
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
    coercion: Option<&LiteralCoercion>,
    span: Span,
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    emit_literal_eq(
        value,
        coercion,
        span,
        subject,
        ctx,
        block,
        &mut output.diagnostics,
    )
    .expect("IR lower: typecheck must have rejected non-lowerable literal")
}
