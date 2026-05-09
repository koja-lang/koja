//! Local-slot emission: `LocalDecl`, `LocalRead`, `LocalWrite`, and
//! the type-keyed `DropLocal` dispatcher. Heap payloads (`String`,
//! `Binary`, `Bits`) free through their `payload-8` block-base
//! header; closure-typed slots delegate to
//! [`super::closures::emit_drop_closure_env`].

use expo_alpha_ir::{IRLocalId, IRType};
use inkwell::values::BasicValueEnum;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::runtime::declare_free_extern;
use crate::types::ir_basic_type;

use super::{closures, inkwell_err};

/// Materialize a `LocalDecl` as an entry-block `alloca`, stashed on
/// the [`EmitContext`] keyed by [`IRLocalId`] for later `load` / `store`.
pub(super) fn emit_local_decl<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    let llvm_ty = ir_basic_type(ctx, ty)?;
    let name = local.to_string();
    let slot = ctx.build_entry_alloca(llvm_ty, &name);
    ctx.register_local_slot(local, slot);
    Ok(())
}

/// Lower a `LocalRead` to an LLVM `load`. Pointer comes from the
/// per-function slot table; load type comes from the IR's static
/// type slot.
pub(super) fn emit_local_read<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let slot = ctx.local_slot(local);
    let llvm_ty = ir_basic_type(ctx, ty)?;
    ctx.builder
        .build_load(llvm_ty, slot, &local.to_string())
        .map_err(|e| inkwell_err(format_args!("build_load for `{local}`"), e))
}

/// Lower a `LocalWrite` to an LLVM `store` into the slot table's
/// pointer for `local`.
pub(super) fn emit_local_write<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let slot = ctx.local_slot(local);
    ctx.builder
        .build_store(slot, value)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_store for `{local}`"), e))
}

/// `String`, `Binary`, and `Bits` all share the single bit-length-
/// header layout (`[i64 bit_length][payload]` with the SSA pointer
/// at the payload), so a single GEP-by-`-8` + `free` shape covers
/// all three. `Function`-typed slots delegate to the closure
/// drop helper. Non-heap types panic loudly: the lowerer is
/// responsible for never emitting `DropLocal` for stack types
/// (it keys on [`IRType`] in `is_heap_type`).
pub(super) fn emit_drop_local<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => {
            let payload = emit_local_read(ctx, local, ty)?;
            let payload_ptr = payload.into_pointer_value();
            let i8_type = ctx.context.i8_type();
            let i64_type = ctx.context.i64_type();
            let block_base = unsafe {
                ctx.builder.build_gep(
                    i8_type,
                    payload_ptr,
                    &[i64_type.const_int((-8i64) as u64, true)],
                    &format!("{local}.block_base"),
                )
            }
            .map_err(|e| inkwell_err(format_args!("drop block-base GEP for `{local}`"), e))?;
            let free = declare_free_extern(ctx);
            ctx.builder
                .build_call(free, &[block_base.into()], &format!("{local}.free"))
                .map_err(|e| inkwell_err(format_args!("free call for `{local}`"), e))?;
            Ok(())
        }
        IRType::Function { .. } => {
            let value = emit_local_read(ctx, local, ty)?;
            closures::emit_drop_closure_env(ctx, local, value)
        }
        _ => panic!(
            "alpha LLVM emit: unsupported `IRInstruction::DropLocal` type {ty:?} for slot `{local}` — \
             extend `emit_drop_local` when more heap types ship",
        ),
    }
}
