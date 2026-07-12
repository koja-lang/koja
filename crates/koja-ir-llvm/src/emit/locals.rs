//! Local-slot emission: `LocalDecl`, `LocalRead`, `LocalWrite`, and
//! the type-keyed `DropLocal` dispatcher. Heap payloads (`String`,
//! `Binary`, `Bits`) free through their `payload-8` block-base
//! header. Closure-typed slots delegate to
//! [`super::closures::emit_drop_closure_env`].

use inkwell::values::BasicValueEnum;
use koja_ir::{IRLocalId, IRType, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::declare_rc_dec_extern;
use crate::types::ir_basic_type;

use super::heap_layout::block_base;
use super::{ValueMap, closures, lookup};

/// Materialize a `LocalDecl` as an entry-block `alloca`, stashed on
/// the [`EmitContext`] keyed by [`IRLocalId`] for later `load` /
/// `store`. TCO bodies pre-register every slot (see
/// [`crate::function::define_function`]), in which case the existing
/// alloca is reused.
///
/// The slot is zero-initialized *at the decl site* (not the hoisted
/// alloca), so re-entering the declaring block (a loop body, a TCO
/// iteration) restores the fresh-slot state. Exit drops therefore
/// always see either a live value or zero, and dropping zero is a
/// no-op (null-safe rc primitives + null-propagating
/// [`block_base`]). This is what makes it safe to "drop a slot the
/// taken path never wrote", e.g. the payload local of an untaken
/// `receive` arm.
pub(super) fn emit_local_decl<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    let llvm_ty = ir_basic_type(ctx, ty)?;
    let slot = match ctx.try_local_slot(local) {
        Some(existing) => existing,
        None => {
            let slot = ctx.build_entry_alloca(llvm_ty, &local.to_string());
            ctx.register_local_slot(local, slot);
            slot
        }
    };
    ctx.builder
        .build_store(slot, llvm_ty.const_zero())
        .or_ice()
        .map(|_| ())
}

/// Lower a `LocalRead` to an LLVM `load`. Pointer comes from the
/// per-function slot table, load type from the IR's static type
/// slot.
pub(super) fn emit_local_read<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let slot = ctx.local_slot(local);
    let llvm_ty = ir_basic_type(ctx, ty)?;
    ctx.builder
        .build_load(llvm_ty, slot, &local.to_string())
        .or_ice()
}

/// Lower a `LocalWrite` to an LLVM `store` into the slot table's
/// pointer for `local`.
pub(super) fn emit_local_write<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let slot = ctx.local_slot(local);
    ctx.builder.build_store(slot, value).or_ice().map(|_| ())
}

/// `String`, `Binary`, and `Bits` all share the single
/// `[i64 rc][i64 bit_length][payload]` layout (SSA pointer at the
/// payload), so a single block-base GEP + `koja_rc_dec` covers all
/// three: the rc is decremented and the block freed at zero (immortal
/// rodata blocks are skipped by the runtime). `Function`-typed slots
/// delegate to the closure drop helper, and no-glue aggregate slots
/// are a no-op. Collections / boxes panic loudly, because they
/// always carry glue and must have been rewritten to a
/// `Call @drop_T` by `elaborate`.
pub(super) fn emit_drop_local<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => {
            let payload = emit_local_read(ctx, local, ty)?;
            emit_rc_dec(ctx, payload.into_pointer_value(), &local.to_string())
        }
        IRType::Function { .. } => {
            let value = emit_local_read(ctx, local, ty)?;
            closures::emit_drop_closure_env(ctx, local, value)
        }
        // No-glue aggregate slot (every field `Copy`): nothing to
        // release. `elaborate` rewrites the heap-owning composites
        // into a `LocalRead` + `Call @drop_T`, so a scalar aggregate
        // is all that survives as a bare `DropLocal` here.
        IRType::Enum(_) | IRType::Struct(_) | IRType::Union { .. } => Ok(()),
        _ => panic!(
            "LLVM emit: unsupported `IRInstruction::DropLocal` type {ty:?} for slot `{local}`. \
             Collections / boxes always carry glue and must be rewritten to a `Call @drop_T`",
        ),
    }
}

/// Value-keyed analog of [`emit_drop_local`]: rc-decrement the heap
/// payload held by the SSA value `value`. Same block-base + `rc_dec`
/// shape, but the payload pointer comes from the value map rather than
/// a slot alloca. Used by overwrite / discarded-temp drops and
/// `FieldSet`-into-heap-leaf lowering to release a payload.
pub(super) fn emit_drop_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: ValueId,
    ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => {
            let payload = lookup(values, value)?;
            emit_rc_dec(ctx, payload.into_pointer_value(), &value.to_string())
        }
        // Closure: `rc--` on the env (capture release + free at zero
        // lives in `koja_closure_rc_dec`). Same path as the slot-keyed
        // `DropLocal` of a `Function`.
        IRType::Function { .. } => {
            let closure_value = lookup(values, value)?;
            closures::emit_drop_closure_value(ctx, closure_value, &value.to_string())
        }
        // No-glue aggregate value (every field `Copy`): nothing to
        // release. The heap-owning composites are rewritten to a
        // `Call @drop_T` by `elaborate`.
        IRType::Enum(_) | IRType::Struct(_) | IRType::Union { .. } => Ok(()),
        _ => panic!(
            "LLVM emit: unsupported `IRInstruction::DropValue` type {ty:?} for value \
             `{value}`. Collections / boxes always carry glue and must be rewritten to a \
             `Call @drop_T`",
        ),
    }
}

/// Emit a `koja_rc_dec` on the block base of a heap-leaf payload
/// (`payload - HEADER_BYTES`). Shared by the slot- and value-keyed
/// drop paths. `label` names the SSA temps for readable IR.
fn emit_rc_dec<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: inkwell::values::PointerValue<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    let base = block_base(ctx, payload, &format!("{label}.block_base"))?;
    let rc_dec = declare_rc_dec_extern(ctx);
    ctx.builder
        .build_call(rc_dec, &[base.into()], &format!("{label}.rc_dec"))
        .or_ice()
        .map(|_| ())
}
