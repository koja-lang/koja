//! Heap-box / unbox helpers for [`expo_alpha_ir::IRType::Indirect`]
//! field slots. `Indirect(T)` is stored as a pointer to a heap-
//! allocated `T`; constructors malloc + memcpy on write, projectors
//! load through the pointer on read. Pairs with the cycle pass in
//! `expo-alpha-ir/src/cycle.rs`.

use expo_alpha_ir::IRType;
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::declare_malloc_extern;
use crate::types::ir_basic_type;

/// Allocate space for `inner` on the heap, copy `value` into it,
/// return the resulting pointer typed as `ptr`.
pub(super) fn emit_box_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let inner_llvm = ir_basic_type(ctx, inner)?;
    let size = ctx.layouts.target_data.get_abi_size(&inner_llvm);
    let size_value = ctx.context.i64_type().const_int(size, false);
    let malloc = declare_malloc_extern(ctx);
    let raw_ptr = ctx
        .builder
        .build_call(malloc, &[size_value.into()], label)
        .map_err(|e| inkwell_err(format_args!("build_call malloc for `{label}`"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen(format!("malloc returned void for `{label}`")))?
        .into_pointer_value();
    ctx.builder
        .build_store(raw_ptr, value)
        .map_err(|e| inkwell_err(format_args!("build_store for box `{label}`"), e))?;
    Ok(raw_ptr.into())
}

/// Load a `T` value through `ptr` where the IR slot is typed
/// `Indirect(T)`. Caller has already extracted the pointer (e.g.
/// from a struct GEP + load); this just routes through the inner
/// type's LLVM shape.
pub(super) fn emit_unbox_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    ptr: PointerValue<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let inner_llvm = ir_basic_type(ctx, inner)?;
    ctx.builder
        .build_load(inner_llvm, ptr, label)
        .map_err(|e| inkwell_err(format_args!("build_load for unbox `{label}`"), e))
}
