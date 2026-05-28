//! Write-side probe + per-method tails: `Map.put` overwrites or
//! inserts a `(K, V)` pair; `Set.insert` is its single-payload twin.
//! Both share [`emit_insert_probe`], which walks slots until the
//! key matches (→ `update_bb`) or an EMPTY/TOMBSTONE slot is hit
//! (→ `insert_bb`).

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

use super::resize::emit_resize_if_needed;
use super::util::{
    KeyHashOps, TableSnapshot, advance_slot, build_table_struct, call_eq, call_hash, codegen_err,
    entry_pointer, extract_table_fields, nth_param, resolve_key_hash_ops, ret_struct,
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
    let table = extract_table_fields(ctx, function, llvm_function)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let value_val = nth_param(function, llvm_function, 2, "value")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;

    let post = emit_resize_if_needed(ctx, function, llvm_function, layout, &table, &key_ops)?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        key_val,
        &key_ops,
    )?;

    // Update path: dup key found, overwrite value slot.
    ctx.builder.position_at_end(probe.update_bb);
    let val_ptr = unsafe {
        ctx.builder
            .build_gep(
                i8_ty,
                probe.e_ptr,
                &[i64_ty.const_int(layout.key_size, false)],
                "val_ptr",
            )
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(val_ptr, value_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let updated = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        post.length,
        post.capacity,
    )?;
    ret_struct(ctx, function, updated)?;

    // Insert path: empty (or tombstone) slot, write key+value + state.
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, key_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let ins_val_ptr = unsafe {
        ctx.builder
            .build_gep(
                i8_ty,
                ins_ptr,
                &[i64_ty.const_int(layout.key_size, false)],
                "ins_val_ptr",
            )
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(ins_val_ptr, value_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(post.length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, function, inserted)
}

pub(crate) fn emit_set_insert<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let table = extract_table_fields(ctx, function, llvm_function)?;
    let item_val = nth_param(function, llvm_function, 1, "item")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;

    let post = emit_resize_if_needed(ctx, function, llvm_function, layout, &table, &key_ops)?;
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
        function,
        post.entries_ptr,
        post.states_ptr,
        post.length,
        post.capacity,
    )?;
    ret_struct(ctx, function, already)?;

    // Insert path: empty (or tombstone) slot, write entry + state.
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, item_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(post.length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, function, inserted)
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
/// `update_bb` (existing key hit — caller decides what to do) or
/// `insert_bb` (empty/tombstone slot — caller writes the new
/// entry). On entry the builder must sit at a single predecessor;
/// on return it sits at an unspecified position and the caller
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
    let hash_val = call_hash(ctx, function, key_ops.hash_fn, key_val)?;
    let mask = ctx
        .builder
        .build_int_sub(table.capacity, i64_ty.const_int(1, false), "mask")
        .map_err(|e| codegen_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let start_slot = ctx
        .builder
        .build_and(hash_val, mask, "start_slot")
        .map_err(|e| codegen_err(format_args!("build_and for `{}`", function.symbol), e))?;

    let probe_loop_bb = ctx.context.append_basic_block(llvm_function, "probe_loop");
    let check_occ_bb = ctx.context.append_basic_block(llvm_function, "check_occ");
    let compare_key_bb = ctx.context.append_basic_block(llvm_function, "compare_key");
    let update_bb = ctx.context.append_basic_block(llvm_function, "update");
    let insert_bb = ctx.context.append_basic_block(llvm_function, "insert");
    let advance_bb = ctx.context.append_basic_block(llvm_function, "advance");

    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;
    ctx.builder.position_at_end(probe_loop_bb);
    let pidx_phi = ctx
        .builder
        .build_phi(i64_ty, "pidx")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    pidx_phi.add_incoming(&[(&start_slot, entry_block)]);
    let pidx = pidx_phi.as_basic_value().into_int_value();

    let s_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, table.states_ptr, &[pidx], "s_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let s_val = ctx
        .builder
        .build_load(i8_ty, s_ptr, "s_val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let is_empty = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            s_val,
            i8_ty.const_int(STATE_EMPTY, false),
            "is_empty",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(is_empty, insert_bb, check_occ_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(check_occ_bb);
    let is_occ = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            s_val,
            i8_ty.const_int(STATE_OCCUPIED, false),
            "is_occ",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(is_occ, compare_key_bb, insert_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(compare_key_bb);
    let e_ptr = entry_pointer(ctx, function, table.entries_ptr, pidx, layout.entry_size)?;
    let existing_key = ctx
        .builder
        .build_load(key_ops.key_basic_ty, e_ptr, "existing_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let keys_equal = call_eq(ctx, function, key_ops.eq_fn, key_val, existing_key)?;
    ctx.builder
        .build_conditional_branch(keys_equal, update_bb, advance_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(advance_bb);
    let wrapped = advance_slot(ctx, function, pidx, mask)?;
    pidx_phi.add_incoming(&[(&wrapped, advance_bb)]);
    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    Ok(InsertProbe {
        e_ptr,
        insert_bb,
        pidx,
        s_ptr,
        update_bb,
    })
}
