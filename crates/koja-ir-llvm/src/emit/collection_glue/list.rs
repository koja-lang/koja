//! `List<T>` clone / deep-copy / drop glue: the dynamic-array buffer
//! walk. Layout is `{ buf_ptr, len, cap }` (see
//! [`crate::types::list_value_type`]), with elements living off-heap
//! behind `buf_ptr` as a flat `[T; cap]`.

use inkwell::values::{FunctionValue, IntValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::{ElementOp, walk_buffer};
use crate::intrinsics::util::{build_list_struct, extract_int, extract_pointer, nth_struct};
use crate::runtime::{declare_free_extern, declare_malloc_extern, declare_memcpy_extern};
use crate::types::abi_size;

use super::call_ptr;

/// `clone_List<T>` / `deep_copy_List<T>`: copy the backing buffer,
/// then acquire (clone) or deep-copy (process-boundary) every element
/// so the returned list owns independent references.
pub(super) fn copy_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
    op: ElementOp,
) -> Result<(), LlvmError> {
    let self_val = nth_struct(function, llvm_function, 0, "self");
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

    walk_buffer(
        ctx,
        llvm_function,
        op,
        element,
        new_buf,
        len,
        element_size,
        "copy",
    )?;

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
    let self_val = nth_struct(function, llvm_function, 0, "self");
    let buf = extract_pointer(ctx, self_val, 0, "buf")?;
    let len = extract_int(ctx, self_val, 1, "len")?;
    let element_size = element_byte_size(ctx, element)?;

    walk_buffer(
        ctx,
        llvm_function,
        ElementOp::Release,
        element,
        buf,
        len,
        element_size,
        "drop",
    )?;

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
