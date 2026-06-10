//! Explicit numeric conversions out of the hub types — the inverse
//! direction of the implicit `NumericWiden` coercion.
//!
//! `Int.to_<width>(self) -> Result<W, NumericConversionError>` range-checks
//! the `i64` receiver and truncates on success; `UInt64.to_int(self)
//! -> Result<Int, NumericConversionError>` accepts any bit pattern at or
//! below `i64::MAX` (i.e. non-negative under a signed view);
//! `Float.to_float32(self) -> Float32` is a total `fptrunc`.
//!
//! The checked conversions mint `NumericConversionError.OutOfRange` on the
//! failure path. The error enum's symbol is recovered from the
//! `Result` return type's `Err` variant payload, so the emitter
//! stays decoupled from the stdlib's mangling scheme.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};
use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag, IntNarrowTarget, NumericConvert,
};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};

/// `enum Result<T, E>` variant tags — declaration order in
/// `koja/lib/global/src/kernel.koja`.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
const RESULT_ERR_TAG: IRVariantTag = IRVariantTag(1);

pub(super) fn emit_numeric_convert<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    convert: NumericConvert,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let receiver = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "numeric convert intrinsic `{}` missing receiver param",
            function.symbol,
        ))
    })?;

    if matches!(convert, NumericConvert::FloatToFloat32) {
        let narrowed = ctx
            .builder
            .build_float_trunc(receiver.into_float_value(), ctx.context.f32_type(), "f32")
            .or_ice()?;
        ctx.builder.build_return(Some(&narrowed)).or_ice()?;
        return Ok(());
    }

    let result_symbol = match &function.return_type {
        IRType::Enum(symbol) => symbol.clone(),
        other => {
            return Err(LlvmError::Codegen(format!(
                "numeric convert intrinsic `{}` expected a Result-enum return, got `{other:?}`",
                function.symbol,
            )));
        }
    };

    let value = receiver.into_int_value();
    let in_range = emit_range_check(ctx, convert, value)?;
    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(in_range, ok_bb, err_bb)
        .or_ice()?;

    emit_ok_branch(ctx, ok_bb, convert, value, &result_symbol)?;
    emit_err_branch(ctx, err_bb, &result_symbol)
}

/// `min <= value <= max` under signed `i64` comparison. The bounds
/// are inclusive and always span a signed-representable range (the
/// `UInt64`-flavored cases reduce to `value >= 0`).
fn emit_range_check<'ctx>(
    ctx: &EmitContext<'ctx>,
    convert: NumericConvert,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let (min, max) = checked_bounds(convert);
    let i64_ty = ctx.context.i64_type();
    let above_min = ctx
        .builder
        .build_int_compare(
            IntPredicate::SGE,
            value,
            i64_ty.const_int(min as u64, true),
            "above_min",
        )
        .or_ice()?;
    let below_max = ctx
        .builder
        .build_int_compare(
            IntPredicate::SLE,
            value,
            i64_ty.const_int(max as u64, true),
            "below_max",
        )
        .or_ice()?;
    ctx.builder
        .build_and(above_min, below_max, "in_range")
        .or_ice()
}

fn emit_ok_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: BasicBlock<'ctx>,
    convert: NumericConvert,
    value: IntValue<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let target_ty = match convert {
        NumericConvert::IntNarrow(target) => match target {
            IntNarrowTarget::Int8 | IntNarrowTarget::UInt8 => ctx.context.i8_type(),
            IntNarrowTarget::Int16 | IntNarrowTarget::UInt16 => ctx.context.i16_type(),
            IntNarrowTarget::Int32 | IntNarrowTarget::UInt32 => ctx.context.i32_type(),
            IntNarrowTarget::UInt64 => ctx.context.i64_type(),
        },
        NumericConvert::UInt64ToInt => ctx.context.i64_type(),
        NumericConvert::FloatToFloat32 => unreachable!("handled before the checked path"),
    };
    let narrowed = if target_ty == ctx.context.i64_type() {
        value
    } else {
        ctx.builder
            .build_int_truncate(value, target_ty, "narrowed")
            .or_ice()?
    };
    let ok = build_enum_value(ctx, result_symbol, RESULT_OK_TAG, &[narrowed.into()])?;
    ctx.builder.build_return(Some(&ok)).or_ice().map(|_| ())
}

fn emit_err_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: BasicBlock<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let err = build_conversion_error(ctx, result_symbol, "OutOfRange")?;
    ctx.builder.build_return(Some(&err)).or_ice().map(|_| ())
}

/// Build `Result.Err(NumericConversionError.<variant>)` over
/// `result_symbol`. The error enum's symbol comes from the `Result`
/// decl and the variant tag is resolved by name, so neither the
/// stdlib's mangling scheme nor `numeric.koja`'s declaration order
/// is baked in here. Shared with the `parse` intrinsics, whose
/// failures carry the same error enum.
pub(super) fn build_conversion_error<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    variant: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let error_symbol = conversion_error_symbol(ctx, result_symbol)?;
    let tag = ctx.layouts.enum_variant_tag(&error_symbol, variant);
    let error_value = build_enum_value(ctx, &error_symbol, tag, &[])?;
    build_enum_value(ctx, result_symbol, RESULT_ERR_TAG, &[error_value])
}

/// Recover `NumericConversionError`'s symbol from the `Result`'s `Err`
/// variant payload type.
fn conversion_error_symbol<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<IRSymbol, LlvmError> {
    let payload = ctx
        .layouts
        .enum_variant_payload(result_symbol, RESULT_ERR_TAG);
    let IRVariantPayload::Tuple(types) = &payload else {
        return Err(LlvmError::Codegen(format!(
            "`{result_symbol}`'s Err variant payload is not a tuple — stdlib invariant violation",
        )));
    };
    match types.as_slice() {
        [IRType::Enum(symbol)] => Ok(symbol.clone()),
        other => Err(LlvmError::Codegen(format!(
            "`{result_symbol}`'s Err payload should be a single enum (NumericConversionError), \
             got `{other:?}`",
        ))),
    }
}

/// Inclusive signed bounds the receiver must satisfy for the
/// conversion to succeed.
fn checked_bounds(convert: NumericConvert) -> (i64, i64) {
    match convert {
        NumericConvert::FloatToFloat32 => unreachable!("total conversion has no bounds"),
        NumericConvert::IntNarrow(target) => match target {
            IntNarrowTarget::Int8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
            IntNarrowTarget::Int16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
            IntNarrowTarget::Int32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
            IntNarrowTarget::UInt8 => (0, i64::from(u8::MAX)),
            IntNarrowTarget::UInt16 => (0, i64::from(u16::MAX)),
            IntNarrowTarget::UInt32 => (0, i64::from(u32::MAX)),
            // Every non-negative `Int` fits `UInt64`.
            IntNarrowTarget::UInt64 => (0, i64::MAX),
        },
        // A `UInt64` bit pattern fits `Int` iff it is at most
        // `i64::MAX` — i.e. non-negative under the signed view.
        NumericConvert::UInt64ToInt => (0, i64::MAX),
    }
}
