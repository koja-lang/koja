//! Heap-box / unbox helpers for [`koja_ir::IRType::Indirect`]
//! field slots. `Indirect(T)` is stored as a pointer to a heap-
//! allocated `T`. Constructors malloc + memcpy on write, projectors
//! load through the pointer on read. Pairs with the cycle pass in
//! `koja-ir/src/cycle.rs`.

use inkwell::AddressSpace;
use inkwell::values::{BasicValueEnum, PointerValue};
use koja_ir::{IRIndirectSlot, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_free_extern, declare_malloc_extern};
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
        .call_basic(malloc, &[size_value.into()], label)?
        .into_pointer_value();
    ctx.builder.build_store(raw_ptr, value).or_ice()?;
    Ok(raw_ptr.into())
}

/// Load a `T` value through `ptr` where the IR slot is typed
/// `Indirect(T)`. Caller has already extracted the pointer (e.g.
/// from a struct GEP + load). This just routes through the inner
/// type's LLVM shape.
pub(super) fn emit_unbox_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    ptr: PointerValue<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let inner_llvm = ir_basic_type(ctx, inner)?;
    ctx.builder.build_load(inner_llvm, ptr, label).or_ice()
}

pub(super) fn emit_free_indirect<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    slot: &IRIndirectSlot,
) -> Result<(), LlvmError> {
    let pointer = indirect_pointer(ctx, base, slot)?;
    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[pointer.into()], "")
        .or_ice()
        .map(|_| ())
}

pub(super) fn emit_indirect_present<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    slot: &IRIndirectSlot,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let pointer = indirect_pointer(ctx, base, slot)?;
    Ok(ctx
        .builder
        .build_is_not_null(pointer, "indirect_present")
        .or_ice()?
        .into())
}

fn indirect_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    slot: &IRIndirectSlot,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let pointer = match slot {
        IRIndirectSlot::EnumPayload {
            payload_index,
            tag,
            ty,
        } => {
            let outer = ctx.enum_outer_type(ty.mangled());
            let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_indirect_src"));
            ctx.builder.build_store(alloca, base).or_ice()?;
            let (complete, payload_type) = ctx.layouts.enum_variant_types(ty.mangled(), *tag);
            let payload_type = payload_type.unwrap_or_else(|| {
                panic!(
                    "LLVM emit: FreeIndirect on `{ty}.{tag}` without a payload \
                     (IR seal invariant violation)",
                )
            });
            let payload_ptr = ctx
                .builder
                .build_struct_gep(complete, alloca, 2, &format!("{ty}_indirect_payload"))
                .or_ice()?;
            let field_ptr = ctx
                .builder
                .build_struct_gep(
                    payload_type,
                    payload_ptr,
                    *payload_index,
                    &format!("{ty}_indirect_{payload_index}_ptr"),
                )
                .or_ice()?;
            load_box_pointer(ctx, field_ptr, &format!("{ty}_indirect_{payload_index}"))?
        }
        IRIndirectSlot::StructField {
            field_index,
            struct_symbol,
        } => {
            let struct_type = ctx.layouts.struct_type(struct_symbol.mangled());
            let alloca =
                ctx.build_entry_alloca(struct_type, &format!("{struct_symbol}_indirect_src"));
            ctx.builder.build_store(alloca, base).or_ice()?;
            let field_ptr = ctx
                .builder
                .build_struct_gep(
                    struct_type,
                    alloca,
                    *field_index,
                    &format!("{struct_symbol}_indirect_{field_index}_ptr"),
                )
                .or_ice()?;
            load_box_pointer(
                ctx,
                field_ptr,
                &format!("{struct_symbol}_indirect_{field_index}"),
            )?
        }
    };
    Ok(pointer)
}

fn load_box_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    slot: PointerValue<'ctx>,
    label: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let pointer_type = ctx.context.ptr_type(AddressSpace::default());
    Ok(ctx
        .builder
        .build_load(pointer_type, slot, label)
        .or_ice()?
        .into_pointer_value())
}
