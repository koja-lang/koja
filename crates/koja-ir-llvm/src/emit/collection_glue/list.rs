//! `List<T>` clone / deep-copy / drop glue: the dynamic-array buffer
//! walk. Layout is `{ buf_ptr, len, cap }` (see
//! [`crate::types::list_value_type`]), with elements living off-heap
//! behind `buf_ptr` as a flat `[T; cap]`.

use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::{acquire_buffer, deep_copy_buffer, release_buffer};
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::list_value_type;

use super::{ElementCopy, abi_size, call_ptr, extract_int, extract_pointer, nth_struct};

/// `clone_List<T>` / `deep_copy_List<T>`: copy the backing buffer,
/// then acquire (clone) or deep-copy (process-boundary) every element
/// so the returned list owns independent references.
pub(super) fn copy_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
    copy: ElementCopy,
) -> Result<(), LlvmError> {
    let self_val = nth_struct(function, llvm_function, 0)?;
    let src_buf = extract_pointer(ctx, self_val, 0, "src_buf")?;
    let len = extract_int(ctx, self_val, 1, "len")?;
    let element_size = element_byte_size(ctx, element)?;

    let alloc_bytes = ctx
        .builder
        .build_int_mul(len, element_size, "alloc_bytes")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let new_buf = call_ptr(ctx, malloc, &[alloc_bytes.into()], "new_buf")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[new_buf.into(), src_buf.into(), alloc_bytes.into()],
            "",
        )
        .or_ice()?;

    match copy {
        ElementCopy::Acquire => acquire_buffer(
            ctx,
            llvm_function,
            element,
            new_buf,
            len,
            element_size,
            "clone",
        )?,
        ElementCopy::Deep => deep_copy_buffer(
            ctx,
            llvm_function,
            element,
            new_buf,
            len,
            element_size,
            "deep_copy",
        )?,
    }

    let result = build_list_struct(ctx, new_buf, len, len)?;
    ctx.builder.build_return(Some(&result)).or_ice().map(|_| ())
}

/// `drop_List<T>`: release every element, then free the buffer.
pub(super) fn drop_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
) -> Result<(), LlvmError> {
    let self_val = nth_struct(function, llvm_function, 0)?;
    let buf = extract_pointer(ctx, self_val, 0, "buf")?;
    let len = extract_int(ctx, self_val, 1, "len")?;
    let element_size = element_byte_size(ctx, element)?;

    release_buffer(ctx, llvm_function, element, buf, len, element_size, "drop")?;

    let free = declare_free_extern(ctx);
    ctx.builder.build_call(free, &[buf.into()], "").or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
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
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let with_buf = ctx
        .builder
        .build_insert_value(list_ty.get_undef(), buf, 0, "with_buf")
        .or_ice()?
        .into_struct_value();
    let with_len = ctx
        .builder
        .build_insert_value(with_buf, len, 1, "with_len")
        .or_ice()?
        .into_struct_value();
    ctx.builder
        .build_insert_value(with_len, cap, 2, "with_cap")
        .or_ice()
        .map(|s| s.into_struct_value())
}
