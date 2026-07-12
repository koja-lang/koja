//! Binary + unary operator emission. Parallel to
//! `koja-ir-eval/src/ops.rs`: same shape, same arm order, but
//! emitting to LLVM via inkwell instead of stepping in-process.
//!
//! `emit_binary_op` / `emit_unary_op` dispatch on the instruction's
//! `operand_ty` to pick the integer / float / string helper, and
//! key signedness off it inside the integer helpers. Arithmetic
//! faults (integer overflow, zero divisors, `MIN / -1`, non-finite
//! float results) branch through [`emit_fault_guard`] to
//! `__koja_panic`, matching the eval interpreter's
//! `RuntimeError::Panicked` messages verbatim.

use inkwell::intrinsics::Intrinsic;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};
use koja_ir::{BinarySign, IRBinOp, IRType, IRUnaryOp, NEG_OVERFLOW_MESSAGE};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_panic_extern, declare_string_eq_extern};
use crate::types::ir_int_type;

use super::constants::emit_string_literal_payload;

pub(super) fn emit_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    operand_ty: &IRType,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match operand_ty {
        IRType::Float32 | IRType::Float64 => {
            emit_float_binary_op(ctx, op, lhs.into_float_value(), rhs.into_float_value())
        }
        IRType::String => {
            emit_string_binary_op(ctx, op, lhs.into_pointer_value(), rhs.into_pointer_value())
        }
        _ => emit_int_binary_op(
            ctx,
            op,
            operand_ty,
            lhs.into_int_value(),
            rhs.into_int_value(),
        ),
    }
}

/// Branch to a panic block calling `__koja_panic(message)` when
/// `fault` is true, then reposition the builder at the fall-through
/// block. Shared by every arithmetic fault site (operators, shift
/// counts, FFI float returns).
pub(crate) fn emit_fault_guard<'ctx>(
    ctx: &EmitContext<'ctx>,
    fault: IntValue<'ctx>,
    message: &str,
    label: &str,
) -> Result<(), LlvmError> {
    let function = ctx
        .builder
        .get_insert_block()
        .and_then(|block| block.get_parent())
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: fault guard `{label}` emitted outside a function body",
            ))
        })?;
    let panic_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_panic"));
    let continue_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_ok"));
    ctx.builder
        .build_conditional_branch(fault, panic_block, continue_block)
        .or_ice()?;

    ctx.builder.position_at_end(panic_block);
    let payload = emit_string_literal_payload(ctx, message.as_bytes(), label);
    let panic = declare_panic_extern(ctx);
    ctx.builder
        .build_call(panic, &[payload.into()], "")
        .or_ice()?;
    ctx.builder.build_unreachable().or_ice()?;

    ctx.builder.position_at_end(continue_block);
    Ok(())
}

/// Integer arithmetic traps on overflow via the
/// `llvm.{s,u}{add,sub,mul}.with.overflow` intrinsics. `Div` /
/// `Mod` guard the zero divisor and (signed only) `MIN / -1`.
/// Comparison predicates and div/rem instructions follow the
/// operand type's signedness. Match-arm order tracks `IRBinOp`'s
/// declaration order in [`koja_ir`].
fn emit_int_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    operand_ty: &IRType,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let signed = operand_ty.int_sign() != Some(BinarySign::Unsigned);
    let result: IntValue<'ctx> = match op {
        IRBinOp::Add | IRBinOp::Mul | IRBinOp::Sub => {
            emit_overflow_checked_arith(ctx, op, signed, lhs, rhs)?
        }
        IRBinOp::Div | IRBinOp::Mod => emit_guarded_div_rem(ctx, op, signed, lhs, rhs)?,
        IRBinOp::Eq => ctx
            .builder
            .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq")
            .or_ice()?,
        IRBinOp::Gt | IRBinOp::GtEq | IRBinOp::Lt | IRBinOp::LtEq => ctx
            .builder
            .build_int_compare(int_predicate(op, signed), lhs, rhs, "cmp")
            .or_ice()?,
        IRBinOp::NotEq => ctx
            .builder
            .build_int_compare(IntPredicate::NE, lhs, rhs, "neq")
            .or_ice()?,
    };
    Ok(result.into())
}

fn int_predicate(op: IRBinOp, signed: bool) -> IntPredicate {
    match (op, signed) {
        (IRBinOp::Gt, true) => IntPredicate::SGT,
        (IRBinOp::Gt, false) => IntPredicate::UGT,
        (IRBinOp::GtEq, true) => IntPredicate::SGE,
        (IRBinOp::GtEq, false) => IntPredicate::UGE,
        (IRBinOp::Lt, true) => IntPredicate::SLT,
        (IRBinOp::Lt, false) => IntPredicate::ULT,
        (IRBinOp::LtEq, true) => IntPredicate::SLE,
        (IRBinOp::LtEq, false) => IntPredicate::ULE,
        (other, _) => unreachable!("int_predicate called with non-ordering op {other:?}"),
    }
}

/// `Add` / `Sub` / `Mul` through the matching
/// `llvm.{s,u}<op>.with.overflow.iN` intrinsic, trapping on the
/// overflow flag.
fn emit_overflow_checked_arith<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    signed: bool,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let name = match (op, signed) {
        (IRBinOp::Add, true) => "llvm.sadd.with.overflow",
        (IRBinOp::Add, false) => "llvm.uadd.with.overflow",
        (IRBinOp::Mul, true) => "llvm.smul.with.overflow",
        (IRBinOp::Mul, false) => "llvm.umul.with.overflow",
        (IRBinOp::Sub, true) => "llvm.ssub.with.overflow",
        (IRBinOp::Sub, false) => "llvm.usub.with.overflow",
        (other, _) => unreachable!("overflow-checked arith called with {other:?}"),
    };
    let declaration = Intrinsic::find(name)
        .and_then(|intrinsic| intrinsic.get_declaration(&ctx.module, &[lhs.get_type().into()]))
        .ok_or_else(|| LlvmError::Codegen(format!("LLVM emit: `{name}` intrinsic unavailable")))?;
    let pair = ctx
        .call_basic(declaration, &[lhs.into(), rhs.into()], "checked")?
        .into_struct_value();
    let result = ctx
        .builder
        .build_extract_value(pair, 0, "arith")
        .or_ice()?
        .into_int_value();
    let overflowed = ctx
        .builder
        .build_extract_value(pair, 1, "overflowed")
        .or_ice()?
        .into_int_value();
    emit_fault_guard(ctx, overflowed, &op.overflow_message(), "overflow")?;
    Ok(result)
}

/// `Div` / `Mod` with a zero-divisor trap, plus the signed
/// `MIN / -1` overflow trap (both are UB on raw `sdiv` / `srem`).
fn emit_guarded_div_rem<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    signed: bool,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let int_type = lhs.get_type();
    let zero_divisor = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, rhs, int_type.const_zero(), "zero_divisor")
        .or_ice()?;
    emit_fault_guard(
        ctx,
        zero_divisor,
        &op.division_by_zero_message(),
        "div_zero",
    )?;

    if !signed {
        return match op {
            IRBinOp::Div => ctx.builder.build_int_unsigned_div(lhs, rhs, "udiv"),
            IRBinOp::Mod => ctx.builder.build_int_unsigned_rem(lhs, rhs, "urem"),
            other => unreachable!("guarded div/rem called with {other:?}"),
        }
        .or_ice();
    }

    let min = int_type.const_int(1u64 << (int_type.get_bit_width() - 1), false);
    let lhs_is_min = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, lhs, min, "lhs_is_min")
        .or_ice()?;
    let rhs_is_neg_one = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            rhs,
            int_type.const_all_ones(),
            "rhs_is_m1",
        )
        .or_ice()?;
    let min_over_neg_one = ctx
        .builder
        .build_and(lhs_is_min, rhs_is_neg_one, "min_over_m1")
        .or_ice()?;
    emit_fault_guard(
        ctx,
        min_over_neg_one,
        &op.overflow_message(),
        "div_overflow",
    )?;
    match op {
        IRBinOp::Div => ctx.builder.build_int_signed_div(lhs, rhs, "div"),
        IRBinOp::Mod => ctx.builder.build_int_signed_rem(lhs, rhs, "mod"),
        other => unreachable!("guarded div/rem called with {other:?}"),
    }
    .or_ice()
}

/// Float arithmetic traps when the IEEE result is non-finite,
/// upholding the finite-only `Float` invariant. Comparisons use
/// **ordered** predicates — with NaN unrepresentable they are total.
fn emit_float_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    lhs: FloatValue<'ctx>,
    rhs: FloatValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let result = match op {
        IRBinOp::Add => ctx.builder.build_float_add(lhs, rhs, "fadd").or_ice()?,
        IRBinOp::Div => ctx.builder.build_float_div(lhs, rhs, "fdiv").or_ice()?,
        IRBinOp::Mod => ctx.builder.build_float_rem(lhs, rhs, "frem").or_ice()?,
        IRBinOp::Mul => ctx.builder.build_float_mul(lhs, rhs, "fmul").or_ice()?,
        IRBinOp::Sub => ctx.builder.build_float_sub(lhs, rhs, "fsub").or_ice()?,
        IRBinOp::Eq
        | IRBinOp::Gt
        | IRBinOp::GtEq
        | IRBinOp::Lt
        | IRBinOp::LtEq
        | IRBinOp::NotEq => {
            return ctx
                .builder
                .build_float_compare(float_predicate(op), lhs, rhs, "fcmp")
                .or_ice()
                .map(Into::into);
        }
    };
    emit_finite_guard(ctx, result, &op.non_finite_message())?;
    Ok(result.into())
}

/// Trap when `value` is NaN or ±inf.
pub(crate) fn emit_finite_guard<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: FloatValue<'ctx>,
    message: &str,
) -> Result<(), LlvmError> {
    let finite = emit_is_finite(ctx, value)?;
    let non_finite = ctx.builder.build_not(finite, "non_finite").or_ice()?;
    emit_fault_guard(ctx, non_finite, message, "non_finite")
}

/// `i1` flag that is true exactly when `value` is finite.
/// `fabs(value) olt +inf` is false for ±inf and (being ordered)
/// for NaN.
pub(crate) fn emit_is_finite<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: FloatValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let float_type = value.get_type();
    let declaration = Intrinsic::find("llvm.fabs")
        .and_then(|intrinsic| intrinsic.get_declaration(&ctx.module, &[float_type.into()]))
        .ok_or_else(|| {
            LlvmError::Codegen("LLVM emit: `llvm.fabs` intrinsic unavailable".to_string())
        })?;
    let magnitude = ctx
        .call_basic(declaration, &[value.into()], "magnitude")?
        .into_float_value();
    ctx.builder
        .build_float_compare(
            FloatPredicate::OLT,
            magnitude,
            float_type.const_float(f64::INFINITY),
            "finite",
        )
        .or_ice()
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
/// source / target pairing is typecheck-guaranteed (sized numeric ->
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
    operand_ty: &IRType,
    operand: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match operand_ty {
        IRType::Float32 | IRType::Float64 => {
            emit_float_unary_op(ctx, op, operand.into_float_value())
        }
        _ => emit_int_unary_op(ctx, op, operand.into_int_value()),
    }
}

/// `Neg` traps on the type's minimum (two's-complement `-MIN`
/// overflows). Typecheck rejects unsigned operands. `Not` is
/// `xor x, -1`, and the seal pass only flows it for `Bool`, so
/// `i1` logical-not falls out for free.
fn emit_int_unary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRUnaryOp,
    operand: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let result: IntValue<'ctx> = match op {
        IRUnaryOp::Neg => {
            let int_type = operand.get_type();
            let min = int_type.const_int(1u64 << (int_type.get_bit_width() - 1), false);
            let is_min = ctx
                .builder
                .build_int_compare(IntPredicate::EQ, operand, min, "is_min")
                .or_ice()?;
            emit_fault_guard(ctx, is_min, NEG_OVERFLOW_MESSAGE, "neg_overflow")?;
            ctx.builder.build_int_neg(operand, "neg")
        }
        IRUnaryOp::Not => ctx.builder.build_not(operand, "not"),
    }
    .or_ice()?;
    Ok(result.into())
}

/// Calls the runtime's length-aware string equality helper. Only
/// `Eq` / `NotEq` admit string operands.
fn emit_string_binary_op<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: IRBinOp,
    lhs: PointerValue<'ctx>,
    rhs: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let predicate = match op {
        IRBinOp::Eq => IntPredicate::NE,
        IRBinOp::NotEq => IntPredicate::EQ,
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: `{other:?}` is not defined for `String` operands \
                 (typecheck violation)",
            )));
        }
    };
    let string_eq = declare_string_eq_extern(ctx);
    let equal = ctx
        .call_basic(string_eq, &[lhs.into(), rhs.into()], "string_eq")?
        .into_int_value();
    let zero = ctx.context.i64_type().const_zero();
    let result = ctx
        .builder
        .build_int_compare(predicate, equal, zero, "streq")
        .or_ice()?;
    Ok(result.into())
}

/// `Neg` is the only float-applicable unary and never traps, since
/// every finite float has a representable negative. `Not` is
/// Bool-only and rejects (typecheck guarantees Bool operands never
/// reach here).
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
