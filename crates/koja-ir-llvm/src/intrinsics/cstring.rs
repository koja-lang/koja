//! Checked `CString.to_string` conversion.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_alloc_size, init_heap_block};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::result;
use crate::runtime::{declare_malloc_extern, declare_utf8_validate_extern};

pub(super) fn emit_to_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let i64_ty = ctx.context.i64_type();
    let (c_ptr, byte_len) = cstring_fields(ctx, function, llvm_function)?;
    let result_symbol = result::return_symbol(function)?;
    let zero = i64_ty.const_zero();

    let negative = ctx
        .builder
        .build_int_compare(IntPredicate::SLT, byte_len, zero, "negative_len")
        .or_ice()?;
    reject_or_continue(
        ctx,
        llvm_function,
        negative,
        result_symbol,
        "invalid_length",
        "nonnegative",
        "InvalidLength",
    )?;

    let is_null = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            c_ptr,
            c_ptr.get_type().const_null(),
            "is_null",
        )
        .or_ice()?;
    let has_bytes = ctx
        .builder
        .build_int_compare(IntPredicate::NE, byte_len, zero, "has_bytes")
        .or_ice()?;
    let invalid_pointer = ctx
        .builder
        .build_and(is_null, has_bytes, "invalid_pointer")
        .or_ice()?;
    reject_or_continue(
        ctx,
        llvm_function,
        invalid_pointer,
        result_symbol,
        "null_pointer",
        "readable",
        "NullPointer",
    )?;

    let validate = declare_utf8_validate_extern(ctx);
    let valid_utf8 = ctx
        .call_basic(validate, &[c_ptr.into(), byte_len.into()], "valid_utf8")?
        .into_int_value();
    let invalid_utf8 = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, valid_utf8, zero, "invalid_utf8")
        .or_ice()?;
    reject_or_continue(
        ctx,
        llvm_function,
        invalid_utf8,
        result_symbol,
        "invalid_utf8",
        "valid",
        "InvalidUTF8",
    )?;

    emit_valid_string(ctx, result_symbol, c_ptr, byte_len, has_bytes)
}

fn cstring_fields<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let receiver = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "CString.to_string missing `self` param on `{}`",
            function.symbol,
        ))
    })?;
    let BasicValueEnum::StructValue(receiver) = receiver else {
        return Err(LlvmError::Codegen(format!(
            "CString.to_string expected struct receiver on `{}`, got `{receiver:?}`",
            function.symbol,
        )));
    };
    let ptr = ctx
        .builder
        .build_extract_value(receiver, 0, "cs_ptr")
        .or_ice()?
        .into_pointer_value();
    let len = ctx
        .builder
        .build_extract_value(receiver, 1, "cs_len")
        .or_ice()?
        .into_int_value();
    Ok((ptr, len))
}

fn emit_valid_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    c_ptr: PointerValue<'ctx>,
    byte_len: IntValue<'ctx>,
    has_bytes: IntValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let total = block_alloc_size(ctx, byte_len, true, "total")?;
    let malloc = declare_malloc_extern(ctx);
    let base_ptr: PointerValue<'ctx> = ctx
        .call_basic(malloc, &[total.into()], "base_ptr")?
        .into_pointer_value();

    let bit_len: IntValue<'ctx> = ctx
        .builder
        .build_int_mul(byte_len, i64_ty.const_int(8, false), "bit_len")
        .or_ice()?;
    let payload_ptr = init_heap_block(ctx, base_ptr, bit_len, "cstring_str")?;
    let copy_source = ctx
        .builder
        .build_select(has_bytes, c_ptr, payload_ptr, "copy_source")
        .or_ice()?
        .into_pointer_value();
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[payload_ptr.into(), copy_source.into(), byte_len.into()],
            "",
        )
        .or_ice()?;
    let nul_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(ctx.context.i8_type(), payload_ptr, &[byte_len], "nul_ptr")
            .or_ice()?
    };
    ctx.builder
        .build_store(nul_ptr, ctx.context.i8_type().const_zero())
        .or_ice()?;
    let ok = result::build_ok(ctx, result_symbol, payload_ptr.into())?;
    ctx.builder.build_return(Some(&ok)).or_ice().map(|_| ())
}

fn reject_or_continue<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    rejected: IntValue<'ctx>,
    result_symbol: &IRSymbol,
    error_block_name: &str,
    continuation_name: &str,
    variant: &str,
) -> Result<(), LlvmError> {
    let error_block = ctx
        .context
        .append_basic_block(llvm_function, error_block_name);
    let continuation = ctx
        .context
        .append_basic_block(llvm_function, continuation_name);
    ctx.builder
        .build_conditional_branch(rejected, error_block, continuation)
        .or_ice()?;

    ctx.builder.position_at_end(error_block);
    let error = result::build_unit_error(ctx, result_symbol, variant)?;
    ctx.builder.build_return(Some(&error)).or_ice()?;
    ctx.builder.position_at_end(continuation);
    Ok(())
}
