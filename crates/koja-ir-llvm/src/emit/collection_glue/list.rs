//! `List<T>` clone / drop glue: the dynamic-array buffer walk. Layout
//! is `{ buf_ptr, len, cap }` (see [`crate::types::list_value_type`]);
//! elements live off-heap behind `buf_ptr` as a flat `[T; cap]`.

use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::{acquire_buffer, release_buffer};
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::list_value_type;

use super::{abi_size, call_ptr, extract_int, extract_pointer, nth_struct};

/// `clone_List<T>`: deep-copy the backing buffer and acquire every
/// element so the returned list owns independent references.
pub(super) fn clone_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
) -> Result<(), LlvmError> {
    let self_val = nth_struct(function, llvm_function, 0)?;
    let src_buf = extract_pointer(ctx, function, self_val, 0, "src_buf")?;
    let len = extract_int(ctx, function, self_val, 1, "len")?;
    let element_size = element_byte_size(ctx, element)?;

    let alloc_bytes = ctx
        .builder
        .build_int_mul(len, element_size, "alloc_bytes")
        .map_err(|e| inkwell_err(format_args!("clone_list mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let new_buf = call_ptr(ctx, function, malloc, &[alloc_bytes.into()], "new_buf")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[new_buf.into(), src_buf.into(), alloc_bytes.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("clone_list memcpy for `{}`", function.symbol),
                e,
            )
        })?;

    acquire_buffer(
        ctx,
        &function.symbol,
        llvm_function,
        element,
        new_buf,
        len,
        element_size,
        "clone",
    )?;

    let result = build_list_struct(ctx, function, new_buf, len, len)?;
    ctx.builder
        .build_return(Some(&result))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("clone_list ret for `{}`", function.symbol), e))
}

/// `drop_List<T>`: release every element, then free the buffer.
pub(super) fn drop_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
) -> Result<(), LlvmError> {
    let self_val = nth_struct(function, llvm_function, 0)?;
    let buf = extract_pointer(ctx, function, self_val, 0, "buf")?;
    let len = extract_int(ctx, function, self_val, 1, "len")?;
    let element_size = element_byte_size(ctx, element)?;

    release_buffer(
        ctx,
        &function.symbol,
        llvm_function,
        element,
        buf,
        len,
        element_size,
        "drop",
    )?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[buf.into()], "")
        .map_err(|e| inkwell_err(format_args!("drop_list free for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("drop_list ret for `{}`", function.symbol), e))
}

fn element_byte_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(ctx
        .context
        .i64_type()
        .const_int(abi_size(ctx, element)?, false))
}

fn build_list_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let with_buf = ctx
        .builder
        .build_insert_value(list_ty.get_undef(), buf, 0, "with_buf")
        .map_err(|e| inkwell_err(format_args!("list insert buf for `{}`", function.symbol), e))?
        .into_struct_value();
    let with_len = ctx
        .builder
        .build_insert_value(with_buf, len, 1, "with_len")
        .map_err(|e| inkwell_err(format_args!("list insert len for `{}`", function.symbol), e))?
        .into_struct_value();
    ctx.builder
        .build_insert_value(with_len, cap, 2, "with_cap")
        .map(|s| s.into_struct_value())
        .map_err(|e| inkwell_err(format_args!("list insert cap for `{}`", function.symbol), e))
}
