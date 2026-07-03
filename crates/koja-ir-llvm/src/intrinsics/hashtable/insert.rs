//! Write-side probe + per-method tails: `Map.put` overwrites or
//! inserts a `(K, V)` pair, `Set.insert` is its single-payload twin.
//! Both share [`emit_insert_probe`], which walks slots until the
//! key matches (→ `update_bb`) or an EMPTY/TOMBSTONE slot is hit
//! (→ `insert_bb`).

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::{acquire_value, release_in_slot};

use super::resize::emit_resize_if_needed;
use super::util::{
    KeyHashOps, TableSnapshot, advance_slot, build_table_struct, call_eq, call_hash,
    clone_table_buffers, entry_pointer, extract_table_fields, nth_param, resolve_key_hash_ops,
    ret_struct, value_slot,
};
use super::{HashtableLayout, STATE_EMPTY, STATE_OCCUPIED};

pub(crate) fn emit_map_put<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let original = extract_table_fields(ctx, function, llvm_function)?;
    let table = clone_table_buffers(ctx, llvm_function, layout, &original)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let value_val = nth_param(function, llvm_function, 2, "value")?;
    let value_ty = layout.value_ty.ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Map.put missing value type for `{}`",
            function.symbol
        ))
    })?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;

    let post = emit_resize_if_needed(ctx, llvm_function, layout, &table, &key_ops)?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        key_val,
        &key_ops,
    )?;

    // Update path: dup key found, overwrite the value slot. Release
    // the old value the clone acquired, store the acquired incoming
    // value. The matched key stays put (no key acquire / release).
    ctx.builder.position_at_end(probe.update_bb);
    let val_ptr = value_slot(ctx, probe.e_ptr, layout.key_size)?;
    release_in_slot(ctx, value_ty, val_ptr)?;
    let update_value = acquire_value(ctx, value_ty, value_val)?;
    ctx.builder.build_store(val_ptr, update_value).or_ice()?;
    let updated = build_table_struct(
        ctx,
        post.entries_ptr,
        post.states_ptr,
        post.length,
        post.capacity,
    )?;
    ret_struct(ctx, updated)?;

    // Insert path: empty (or tombstone) slot, write key+value + state.
    // Both payloads are acquired so the table owns independent
    // references (the stale bytes a tombstone carries were never
    // acquired, so the overwrite needs no release).
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(ctx, post.entries_ptr, probe.pidx, layout.entry_size)?;
    let insert_key = acquire_value(ctx, layout.key_ty, key_val)?;
    ctx.builder.build_store(ins_ptr, insert_key).or_ice()?;
    let ins_val_ptr = value_slot(ctx, ins_ptr, layout.key_size)?;
    let insert_value = acquire_value(ctx, value_ty, value_val)?;
    ctx.builder
        .build_store(ins_val_ptr, insert_value)
        .or_ice()?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .or_ice()?;
    let new_len = ctx
        .builder
        .build_int_add(post.length, i64_ty.const_int(1, false), "new_len")
        .or_ice()?;
    let inserted = build_table_struct(
        ctx,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, inserted)
}

pub(crate) fn emit_set_insert<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let original = extract_table_fields(ctx, function, llvm_function)?;
    let table = clone_table_buffers(ctx, llvm_function, layout, &original)?;
    let item_val = nth_param(function, llvm_function, 1, "item")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;

    let post = emit_resize_if_needed(ctx, llvm_function, layout, &table, &key_ops)?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        item_val,
        &key_ops,
    )?;

    // Duplicate-key path: Set returns self unchanged (no update).
    ctx.builder.position_at_end(probe.update_bb);
    let already = build_table_struct(
        ctx,
        post.entries_ptr,
        post.states_ptr,
        post.length,
        post.capacity,
    )?;
    ret_struct(ctx, already)?;

    // Insert path: empty (or tombstone) slot, write entry + state. The
    // item is acquired so the set owns an independent reference (a
    // tombstone's stale bytes were never acquired, so no release).
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(ctx, post.entries_ptr, probe.pidx, layout.entry_size)?;
    let insert_item = acquire_value(ctx, layout.key_ty, item_val)?;
    ctx.builder.build_store(ins_ptr, insert_item).or_ice()?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .or_ice()?;
    let new_len = ctx
        .builder
        .build_int_add(post.length, i64_ty.const_int(1, false), "new_len")
        .or_ice()?;
    let inserted = build_table_struct(
        ctx,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, inserted)
}

/// Output of [`emit_insert_probe`]: which `update` vs `insert` block
/// each outcome reached, plus the `pidx` / `e_ptr` / `s_ptr` SSA
/// values the per-collection tail consumes.
pub(super) struct InsertProbe<'ctx> {
    pub e_ptr: PointerValue<'ctx>,
    pub insert_bb: BasicBlock<'ctx>,
    pub pidx: IntValue<'ctx>,
    pub s_ptr: PointerValue<'ctx>,
    pub update_bb: BasicBlock<'ctx>,
}

/// Emit a probe loop that returns to the caller at either
/// `update_bb` (existing key hit, caller decides what to do) or
/// `insert_bb` (empty/tombstone slot, caller writes the new
/// entry). On entry the builder must sit at a single predecessor.
/// On return it sits at an unspecified position and the caller
/// branches to `update_bb` / `insert_bb` via `position_at_end`.
pub(super) fn emit_insert_probe<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    table: &TableSnapshot<'ctx>,
    key_val: BasicValueEnum<'ctx>,
    key_ops: &KeyHashOps<'ctx>,
) -> Result<InsertProbe<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "emit_insert_probe called with no insertion block for `{}`",
            function.symbol,
        ))
    })?;
    let hash_val = call_hash(ctx, key_ops.hash_fn, key_val)?;
    let mask = ctx
        .builder
        .build_int_sub(table.capacity, i64_ty.const_int(1, false), "mask")
        .or_ice()?;
    let start_slot = ctx
        .builder
        .build_and(hash_val, mask, "start_slot")
        .or_ice()?;

    let probe_loop_bb = ctx.context.append_basic_block(llvm_function, "probe_loop");
    let check_occ_bb = ctx.context.append_basic_block(llvm_function, "check_occ");
    let compare_key_bb = ctx.context.append_basic_block(llvm_function, "compare_key");
    let update_bb = ctx.context.append_basic_block(llvm_function, "update");
    let insert_bb = ctx.context.append_basic_block(llvm_function, "insert");
    let advance_bb = ctx.context.append_basic_block(llvm_function, "advance");

    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .or_ice()?;
    ctx.builder.position_at_end(probe_loop_bb);
    let pidx_phi = ctx.builder.build_phi(i64_ty, "pidx").or_ice()?;
    pidx_phi.add_incoming(&[(&start_slot, entry_block)]);
    let pidx = pidx_phi.as_basic_value().into_int_value();

    let s_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, table.states_ptr, &[pidx], "s_ptr")
            .or_ice()?
    };
    let s_val = ctx
        .builder
        .build_load(i8_ty, s_ptr, "s_val")
        .or_ice()?
        .into_int_value();
    let is_empty = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            s_val,
            i8_ty.const_int(STATE_EMPTY, false),
            "is_empty",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(is_empty, insert_bb, check_occ_bb)
        .or_ice()?;

    ctx.builder.position_at_end(check_occ_bb);
    let is_occ = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            s_val,
            i8_ty.const_int(STATE_OCCUPIED, false),
            "is_occ",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(is_occ, compare_key_bb, insert_bb)
        .or_ice()?;

    ctx.builder.position_at_end(compare_key_bb);
    let e_ptr = entry_pointer(ctx, table.entries_ptr, pidx, layout.entry_size)?;
    let existing_key = ctx
        .builder
        .build_load(key_ops.key_basic_ty, e_ptr, "existing_key")
        .or_ice()?;
    let keys_equal = call_eq(ctx, key_ops.eq_fn, key_val, existing_key)?;
    ctx.builder
        .build_conditional_branch(keys_equal, update_bb, advance_bb)
        .or_ice()?;

    ctx.builder.position_at_end(advance_bb);
    let wrapped = advance_slot(ctx, pidx, mask)?;
    pidx_phi.add_incoming(&[(&wrapped, advance_bb)]);
    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .or_ice()?;

    Ok(InsertProbe {
        e_ptr,
        insert_bb,
        pidx,
        s_ptr,
        update_bb,
    })
}
