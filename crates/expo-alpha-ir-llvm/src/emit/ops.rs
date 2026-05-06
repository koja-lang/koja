//! Binary + unary operator emission. Parallel to
//! `expo-alpha-ir-eval/src/ops.rs`: same shape, same arm order, but
//! emitting to LLVM via inkwell instead of stepping in-process.
//!
//! `emit_binary_op` / `emit_unary_op` peek the operand
//! [`BasicValueEnum`] variant to pick the integer / float helper —
//! typecheck guarantees both operands of a binary op agree on
//! numeric shape. Each shape gets its own helper so the per-shape
//! match arms stay exhaustive over the operators that shape owns.
//! Comparisons always return `i1` regardless of operand shape, so
//! the helpers all return `BasicValueEnum` (the dispatcher's public
//! return type) instead of narrow `IntValue` / `FloatValue`.

use expo_alpha_ir::{IRBinOp, IRUnaryOp};
use inkwell::values::{BasicValueEnum, FloatValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::ctx::EmitCtx;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

pub(super) fn emit_binary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRBinOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if lhs.is_float_value() {
        emit_float_binary_op(ctx, op, lhs.into_float_value(), rhs.into_float_value())
    } else {
        emit_int_binary_op(ctx, op, lhs.into_int_value(), rhs.into_int_value())
    }
}

/// Integer arithmetic uses the wrapping `build_int_*` calls (no
/// `nsw`/`nuw`) per Expo's two's-complement overflow contract.
/// Comparisons use signed predicates — the seal pass admits `Int64`
/// only today; threading signedness for `UInt*` is a follow-up.
/// Match-arm order tracks `IRBinOp`'s declaration order in
/// [`expo_alpha_ir`].
fn emit_int_binary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRBinOp,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let result: IntValue<'ctx> = match op {
        IRBinOp::Add => ctx.builder.build_int_add(lhs, rhs, "add"),
        IRBinOp::And => ctx.builder.build_and(lhs, rhs, "and"),
        IRBinOp::Div => ctx.builder.build_int_signed_div(lhs, rhs, "div"),
        IRBinOp::Eq => ctx
            .builder
            .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq"),
        IRBinOp::Gt => ctx
            .builder
            .build_int_compare(IntPredicate::SGT, lhs, rhs, "gt"),
        IRBinOp::GtEq => ctx
            .builder
            .build_int_compare(IntPredicate::SGE, lhs, rhs, "gte"),
        IRBinOp::Lt => ctx
            .builder
            .build_int_compare(IntPredicate::SLT, lhs, rhs, "lt"),
        IRBinOp::LtEq => ctx
            .builder
            .build_int_compare(IntPredicate::SLE, lhs, rhs, "lte"),
        IRBinOp::Mod => ctx.builder.build_int_signed_rem(lhs, rhs, "mod"),
        IRBinOp::Mul => ctx.builder.build_int_mul(lhs, rhs, "mul"),
        IRBinOp::NotEq => ctx
            .builder
            .build_int_compare(IntPredicate::NE, lhs, rhs, "neq"),
        IRBinOp::Or => ctx.builder.build_or(lhs, rhs, "or"),
        IRBinOp::Sub => ctx.builder.build_int_sub(lhs, rhs, "sub"),
    }
    .map_err(|e| inkwell_err(format_args!("emit for {op:?}"), e))?;
    Ok(result.into())
}

/// Float arithmetic + comparisons. `And` / `Or` reject — typecheck
/// keeps Bool-only logic away from this seam. Comparisons use
/// **ordered** predicates (`OEQ`/`OLT`/etc.) so `NaN`-on-either-side
/// returns `false`, matching the `expo-alpha-ir-eval` interpreter
/// and v1 codegen.
fn emit_float_binary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRBinOp,
    lhs: FloatValue<'ctx>,
    rhs: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let to_err = |e| inkwell_err(format_args!("emit for {op:?}"), e);
    match op {
        IRBinOp::Add => ctx
            .builder
            .build_float_add(lhs, rhs, "fadd")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::Div => ctx
            .builder
            .build_float_div(lhs, rhs, "fdiv")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::Mod => ctx
            .builder
            .build_float_rem(lhs, rhs, "frem")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::Mul => ctx
            .builder
            .build_float_mul(lhs, rhs, "fmul")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::Sub => ctx
            .builder
            .build_float_sub(lhs, rhs, "fsub")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::Eq
        | IRBinOp::Gt
        | IRBinOp::GtEq
        | IRBinOp::Lt
        | IRBinOp::LtEq
        | IRBinOp::NotEq => ctx
            .builder
            .build_float_compare(float_predicate(op), lhs, rhs, "fcmp")
            .map(Into::into)
            .map_err(to_err),
        IRBinOp::And | IRBinOp::Or => Err(LlvmError::Codegen(format!(
            "alpha LLVM emit: `{op:?}` is Bool-only — float operands should never reach this \
             path (typecheck violation)",
        ))),
    }
}

fn float_predicate(op: IRBinOp) -> FloatPredicate {
    match op {
        IRBinOp::Eq => FloatPredicate::OEQ,
        IRBinOp::Gt => FloatPredicate::OGT,
        IRBinOp::GtEq => FloatPredicate::OGE,
        IRBinOp::Lt => FloatPredicate::OLT,
        IRBinOp::LtEq => FloatPredicate::OLE,
        IRBinOp::NotEq => FloatPredicate::ONE,
        other => unreachable!("float_predicate called with non-comparison op {other:?}"),
    }
}

/// Unary op dispatcher: `Neg` on float operands routes to
/// `build_float_neg`; integer / `Bool` operands keep the int helper.
pub(super) fn emit_unary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRUnaryOp,
    operand: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if operand.is_float_value() {
        emit_float_unary_op(ctx, op, operand.into_float_value())
    } else {
        emit_int_unary_op(ctx, op, operand.into_int_value())
    }
}

/// `Neg` wraps on `i64::MIN` (the eval interpreter's `checked_neg`
/// trap is a known divergence). `Not` is `xor x, -1`; the seal pass
/// only flows `Not` for `Bool`, so `i1` logical-not falls out for
/// free.
fn emit_int_unary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRUnaryOp,
    operand: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let result: IntValue<'ctx> = match op {
        IRUnaryOp::Neg => ctx.builder.build_int_neg(operand, "neg"),
        IRUnaryOp::Not => ctx.builder.build_not(operand, "not"),
    }
    .map_err(|e| inkwell_err(format_args!("emit for {op:?}"), e))?;
    Ok(result.into())
}

/// `Neg` is the only float-applicable unary; `Not` is Bool-only and
/// rejects (typecheck guarantees Bool operands never reach here).
fn emit_float_unary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRUnaryOp,
    operand: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match op {
        IRUnaryOp::Neg => ctx
            .builder
            .build_float_neg(operand, "fneg")
            .map(Into::into)
            .map_err(|e| inkwell_err("emit for fneg", e)),
        IRUnaryOp::Not => Err(LlvmError::Codegen(
            "alpha LLVM emit: `not` is Bool-only — float operand should never reach this path \
             (typecheck violation)"
                .to_string(),
        )),
    }
}
