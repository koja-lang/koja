//! Binary + unary operator emission. Parallel to
//! `koja-ir-eval/src/ops.rs`: same shape, same arm order, but
//! emitting to LLVM via inkwell instead of stepping in-process.
//!
//! `emit_binary_op` / `emit_unary_op` peek the operand
//! [`BasicValueEnum`] variant to pick the integer / float helper.
//! Typecheck guarantees both operands of a binary op agree on
//! numeric shape. Each shape gets its own helper so the per-shape
//! match arms stay exhaustive over the operators that shape owns.
//! Comparisons always return `i1` regardless of operand shape, so
//! the helpers all return `BasicValueEnum` (the dispatcher's public
//! return type) instead of narrow `IntValue` / `FloatValue`.

use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};
use koja_ir::{IRBinOp, IRType, IRUnaryOp};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::declare_strcmp_extern;
use crate::types::ir_int_type;

pub(super) fn emit_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if lhs.is_float_value() {
        emit_float_binary_op(ctx, op, lhs.into_float_value(), rhs.into_float_value())
    } else if lhs.is_pointer_value() {
        // `IRType::String` is the only pointer-typed operand
        // typecheck admits in a binary op today (and only for
        // `Eq` / `NotEq`).
        emit_string_binary_op(ctx, op, lhs.into_pointer_value(), rhs.into_pointer_value())
    } else {
        emit_int_binary_op(ctx, op, lhs.into_int_value(), rhs.into_int_value())
    }
}

/// Integer arithmetic uses the wrapping `build_int_*` calls (no
/// `nsw`/`nuw`) per Koja's two's-complement overflow contract.
/// Comparisons use signed predicates. The seal pass admits `Int64`
/// only today, and threading signedness for `UInt*` is a follow-up.
/// Match-arm order tracks `IRBinOp`'s declaration order in
/// [`koja_ir`].
fn emit_int_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
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
    .or_ice()?;
    Ok(result.into())
}

/// Float arithmetic + comparisons. `And` / `Or` reject because
/// typecheck keeps Bool-only logic away from this seam. Comparisons use
/// **ordered** predicates (`OEQ`/`OLT`/etc.) so `NaN`-on-either-side
/// returns `false`, matching the `koja-ir-eval` interpreter
/// and v1 codegen.
fn emit_float_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    lhs: FloatValue<'ctx>,
    rhs: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match op {
        IRBinOp::Add => ctx
            .builder
            .build_float_add(lhs, rhs, "fadd")
            .or_ice()
            .map(Into::into),
        IRBinOp::Div => ctx
            .builder
            .build_float_div(lhs, rhs, "fdiv")
            .or_ice()
            .map(Into::into),
        IRBinOp::Mod => ctx
            .builder
            .build_float_rem(lhs, rhs, "frem")
            .or_ice()
            .map(Into::into),
        IRBinOp::Mul => ctx
            .builder
            .build_float_mul(lhs, rhs, "fmul")
            .or_ice()
            .map(Into::into),
        IRBinOp::Sub => ctx
            .builder
            .build_float_sub(lhs, rhs, "fsub")
            .or_ice()
            .map(Into::into),
        IRBinOp::Eq
        | IRBinOp::Gt
        | IRBinOp::GtEq
        | IRBinOp::Lt
        | IRBinOp::LtEq
        | IRBinOp::NotEq => ctx
            .builder
            .build_float_compare(float_predicate(op), lhs, rhs, "fcmp")
            .or_ice()
            .map(Into::into),
        IRBinOp::And | IRBinOp::Or => Err(LlvmError::Codegen(format!(
            "LLVM emit: `{op:?}` is Bool-only, float operands should never reach this \
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

/// Lossless hub widening: sign-extend signed integer sources,
/// zero-extend unsigned, `fpext` a `Float32` into `f64`. The
/// source / target pairing is typecheck-guaranteed (sized numeric →
/// `Int64` / `Float64` only), so any other `from` shape is an ICE.
pub(super) fn emit_numeric_widen<'ctx>(
    ctx: &EmitContext<'ctx>,
    from: &IRType,
    to: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if matches!(from, IRType::Float32) {
        let widened = ctx
            .builder
            .build_float_ext(value.into_float_value(), ctx.context.f64_type(), "fwiden")
            .or_ice()?;
        return Ok(widened.into());
    }
    let target_ty = ir_int_type(ctx.context, to)?;
    let int_value = value.into_int_value();
    let widened = match from {
        IRType::Int8 | IRType::Int16 | IRType::Int32 => ctx
            .builder
            .build_int_s_extend(int_value, target_ty, "swiden"),
        IRType::UInt8 | IRType::UInt16 | IRType::UInt32 => ctx
            .builder
            .build_int_z_extend(int_value, target_ty, "zwiden"),
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: NumericWiden source must be a widenable sized numeric, \
                 got `{other:?}` (typecheck violation)",
            )));
        }
    }
    .or_ice()?;
    Ok(widened.into())
}

/// Unary op dispatcher: `Neg` on float operands routes to
/// `build_float_neg`, integer / `Bool` operands keep the int helper.
pub(super) fn emit_unary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
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
/// trap is a known divergence). `Not` is `xor x, -1`. The seal pass
/// only flows `Not` for `Bool`, so `i1` logical-not falls out for
/// free.
fn emit_int_unary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRUnaryOp,
    operand: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let result: IntValue<'ctx> = match op {
        IRUnaryOp::Neg => ctx.builder.build_int_neg(operand, "neg"),
        IRUnaryOp::Not => ctx.builder.build_not(operand, "not"),
    }
    .or_ice()?;
    Ok(result.into())
}

/// `strcmp(lhs, rhs)` then `icmp` the result against zero. Only
/// `Eq` / `NotEq` admit string operands, because typecheck doesn't
/// admit ordering or arithmetic on strings.
fn emit_string_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    lhs: PointerValue<'ctx>,
    rhs: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let predicate = match op {
        IRBinOp::Eq => IntPredicate::EQ,
        IRBinOp::NotEq => IntPredicate::NE,
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: `{other:?}` is not defined for `String` operands \
                 (typecheck violation)",
            )));
        }
    };
    let strcmp = declare_strcmp_extern(ctx);
    let diff = ctx
        .call_basic(strcmp, &[lhs.into(), rhs.into()], "strcmp")?
        .into_int_value();
    let zero = ctx.context.i32_type().const_zero();
    let result = ctx
        .builder
        .build_int_compare(predicate, diff, zero, "streq")
        .or_ice()?;
    Ok(result.into())
}

/// `Neg` is the only float-applicable unary. `Not` is Bool-only and
/// rejects (typecheck guarantees Bool operands never reach here).
fn emit_float_unary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRUnaryOp,
    operand: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match op {
        IRUnaryOp::Neg => ctx
            .builder
            .build_float_neg(operand, "fneg")
            .or_ice()
            .map(Into::into),
        IRUnaryOp::Not => Err(LlvmError::Codegen(
            "LLVM emit: `not` is Bool-only, float operand should never reach this path \
             (typecheck violation)"
                .to_string(),
        )),
    }
}
