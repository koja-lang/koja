//! `List<T>` clone / drop glue: the dynamic-array buffer walk. Layout
//! is `{ buf_ptr, len, cap }` (see [`crate::types::list_value_type`]);
//! elements live off-heap behind `buf_ptr` as a flat `[T; cap]`.

use inkwell::IntPredicate;
use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::list_value_type;

use super::{
    abi_size, acquire_element, call_ptr, element_slot, extract_int, extract_pointer, nth_struct,
    release_element,
};

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

    emit_index_loop(ctx, llvm_function, len, "clone", |ctx, index| {
        let slot = element_slot(ctx, function, new_buf, index, element_size)?;
        acquire_element(ctx, function, element, slot)
    })?;

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

    emit_index_loop(ctx, llvm_function, len, "drop", |ctx, index| {
        let slot = element_slot(ctx, function, buf, index, element_size)?;
        release_element(ctx, function, element, slot)
    })?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[buf.into()], "")
        .map_err(|e| inkwell_err(format_args!("drop_list free for `{}`", function.symbol), e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("drop_list ret for `{}`", function.symbol), e))
}

/// Emit a `for index in 0..count` loop whose straight-line `body` is
/// generated once into the loop body block. The body must not branch
/// (it emits into and leaves control in the body block); the helper
/// owns the counter, the `index < count` guard, and the back-edge.
fn emit_index_loop<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    count: IntValue<'ctx>,
    label: &str,
    body: impl FnOnce(&EmitContext<'ctx>, IntValue<'ctx>) -> Result<(), LlvmError>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let counter = ctx.build_entry_alloca(i64_ty, &format!("{label}.i"));
    ctx.builder
        .build_store(counter, i64_ty.const_zero())
        .map_err(|e| inkwell_err(format_args!("{label} loop counter init"), e))?;
    let head = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.head"));
    let body_block = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.body"));
    let exit = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.exit"));

    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop entry branch"), e))?;
    ctx.builder.position_at_end(head);
    let index = ctx
        .builder
        .build_load(i64_ty, counter, &format!("{label}.idx"))
        .map_err(|e| inkwell_err(format_args!("{label} loop index load"), e))?
        .into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, count, &format!("{label}.cmp"))
        .map_err(|e| inkwell_err(format_args!("{label} loop guard"), e))?;
    ctx.builder
        .build_conditional_branch(in_range, body_block, exit)
        .map_err(|e| inkwell_err(format_args!("{label} loop branch"), e))?;

    ctx.builder.position_at_end(body_block);
    body(ctx, index)?;
    let next = ctx
        .builder
        .build_int_add(index, i64_ty.const_int(1, false), &format!("{label}.inc"))
        .map_err(|e| inkwell_err(format_args!("{label} loop increment"), e))?;
    ctx.builder
        .build_store(counter, next)
        .map_err(|e| inkwell_err(format_args!("{label} loop counter store"), e))?;
    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop back-edge"), e))?;

    ctx.builder.position_at_end(exit);
    Ok(())
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
