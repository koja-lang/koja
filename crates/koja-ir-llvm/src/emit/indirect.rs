//! Heap-box / unbox helpers for [`koja_ir::IRType::Indirect`]
//! field slots. `Indirect(T)` is stored as a pointer to the payload of
//! a reference-counted `[i64 rc][i64 payload_bytes][T]` block (the
//! same header convention as the leaf heap types, so `block_base` and
//! the runtime rc primitives apply unchanged). Boxes are write-once.
//! Constructors allocate + store, clone shares via `rc++`, and the
//! rc-aware release in [`emit_release_box`] frees at rc 1. Projectors
//! load through the pointer on read. Pairs with the cycle pass in
//! `koja-ir/src/cycle.rs`.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, PointerValue};
use koja_ir::{IRIndirectSlot, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::release_in_slot;
use crate::runtime::{declare_free_extern, declare_malloc_extern, declare_rc_dec_extern};
use crate::types::ir_basic_type;

use super::heap_layout::{block_alloc_size, block_base, init_heap_block};

/// Allocate a fresh rc block for `inner` on the heap (rc stamped 1),
/// copy `value` into its payload, and return the payload pointer.
pub(super) fn emit_box_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let inner_llvm = ir_basic_type(ctx, inner)?;
    let size = ctx.layouts.target_data.get_abi_size(&inner_llvm);
    let size_value = ctx.context.i64_type().const_int(size, false);
    let total = block_alloc_size(ctx, size_value, false, &format!("{label}_size"))?;
    let malloc = declare_malloc_extern(ctx);
    let base = ctx
        .call_basic(malloc, &[total.into()], label)?
        .into_pointer_value();
    let payload = init_heap_block(ctx, base, size_value, label)?;
    ctx.builder.build_store(payload, value).or_ice()?;
    Ok(payload.into())
}

/// Release one reference to the box whose payload pointer is
/// `payload` (typed `Indirect(inner)`). Null is a no-op. At rc 1 the
/// contents are released through `inner`'s drop path and the block is
/// freed. Otherwise `koja_rc_dec` decrements (immortal-safe).
pub(super) fn emit_release_box<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    payload: PointerValue<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    let function = ctx
        .builder
        .get_insert_block()
        .and_then(|block| block.get_parent())
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: box release `{label}` emitted outside a function body",
            ))
        })?;
    let check_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_check"));
    let last_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_last"));
    let dec_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_dec"));
    let done_block = ctx
        .context
        .append_basic_block(function, &format!("{label}_done"));

    let is_null = ctx
        .builder
        .build_is_null(payload, &format!("{label}_is_null"))
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(is_null, done_block, check_block)
        .or_ice()?;

    ctx.builder.position_at_end(check_block);
    let base = block_base(ctx, payload, &format!("{label}_base"))?;
    let rc = ctx
        .builder
        .build_load(ctx.context.i64_type(), base, &format!("{label}_rc"))
        .or_ice()?
        .into_int_value();
    let unique = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            rc,
            ctx.context.i64_type().const_int(1, false),
            &format!("{label}_unique"),
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(unique, last_block, dec_block)
        .or_ice()?;

    // Last owner. Release the contents (the payload pointer doubles
    // as a `T` slot), then free the block.
    ctx.builder.position_at_end(last_block);
    release_in_slot(ctx, inner, payload)?;
    let free = declare_free_extern(ctx);
    ctx.builder.build_call(free, &[base.into()], "").or_ice()?;
    ctx.builder
        .build_unconditional_branch(done_block)
        .or_ice()?;

    // Shared (or immortal), so decrement and leave the block alive.
    ctx.builder.position_at_end(dec_block);
    let rc_dec = declare_rc_dec_extern(ctx);
    ctx.builder
        .build_call(rc_dec, &[base.into()], "")
        .or_ice()?;
    ctx.builder
        .build_unconditional_branch(done_block)
        .or_ice()?;

    ctx.builder.position_at_end(done_block);
    Ok(())
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
                    "LLVM emit: IndirectPresent on `{ty}.{tag}` without a payload \
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
