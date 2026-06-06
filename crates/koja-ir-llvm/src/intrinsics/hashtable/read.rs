//! Read-only methods built on a shared probe loop: `Map.get`,
//! `Map.has?` / `Set.has?`, and `Map.remove` / `Set.remove`. The
//! probe walks slots until it lands on a state-EMPTY (miss) or
//! finds an OCCUPIED slot whose key compares equal; the per-method
//! tails branch off the same `found_bb` / `not_found_bb` pair.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PhiValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

use super::util::{
    KeyHashOps, TableSnapshot, advance_slot, build_table_struct, call_eq, call_hash,
    clone_table_buffers, codegen_err, entry_pointer, expect_enum_symbol, extract_int,
    extract_pointer, extract_table_fields, nth_hashtable, nth_param, resolve_key_hash_ops,
    ret_basic, ret_struct,
};
use super::{
    HashtableLayout, OPTION_NONE_TAG, OPTION_SOME_TAG, STATE_EMPTY, STATE_OCCUPIED, STATE_TOMBSTONE,
};

/// Output of [`emit_read_only_probe`]. Caller positions at
/// `found_bb` or `not_found_bb` to emit the per-method outcome.
/// `pidx` / `s_ptr` / `e_ptr` are valid in `found_bb`.
struct ReadOnlyProbe<'ctx> {
    advance_bb: BasicBlock<'ctx>,
    e_ptr: PointerValue<'ctx>,
    found_bb: BasicBlock<'ctx>,
    not_found_bb: BasicBlock<'ctx>,
    pidx: IntValue<'ctx>,
    pidx_phi: PhiValue<'ctx>,
    s_ptr: PointerValue<'ctx>,
}

/// Emit the read-only probe loop used by `get` / `has?` / `remove`.
/// Reads `table.entries_ptr`, `table.states_ptr`, `table.capacity`
/// (probe terminates on empty before touching `length`). Builder
/// must be positioned at a predecessor block before calling
/// (typically the entry block); on return the builder sits at an
/// unspecified location and the caller positions itself at the
/// returned `found_bb` / `not_found_bb` blocks to emit the outcome.
/// The `advance` edge wires itself; the caller does **not** need
/// to mutate the returned `pidx_phi` directly (it's exposed so
/// callers can attach extra incoming edges from custom entry-side
/// branching, e.g. `put`'s resize-or-not phi).
fn emit_read_only_probe<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    table: &TableSnapshot<'ctx>,
    key_val: BasicValueEnum<'ctx>,
    key_ops: &KeyHashOps<'ctx>,
) -> Result<ReadOnlyProbe<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "emit_read_only_probe called with no insertion block for `{}`",
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

    let probe_bb = ctx.context.append_basic_block(llvm_function, "probe");
    let check_bb = ctx.context.append_basic_block(llvm_function, "check");
    let cmp_bb = ctx.context.append_basic_block(llvm_function, "cmp");
    let found_bb = ctx.context.append_basic_block(llvm_function, "found");
    let not_found_bb = ctx.context.append_basic_block(llvm_function, "not_found");
    let advance_bb = ctx.context.append_basic_block(llvm_function, "advance");

    ctx.builder
        .build_unconditional_branch(probe_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;
    ctx.builder.position_at_end(probe_bb);
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
        .build_conditional_branch(is_empty, not_found_bb, check_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(check_bb);
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
        .build_conditional_branch(is_occ, cmp_bb, advance_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(cmp_bb);
    let e_ptr = entry_pointer(ctx, function, table.entries_ptr, pidx, layout.entry_size)?;
    let existing_key = ctx
        .builder
        .build_load(key_ops.key_basic_ty, e_ptr, "existing_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let keys_equal = call_eq(ctx, function, key_ops.eq_fn, key_val, existing_key)?;
    ctx.builder
        .build_conditional_branch(keys_equal, found_bb, advance_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(advance_bb);
    let wrapped = advance_slot(ctx, function, pidx, mask)?;
    pidx_phi.add_incoming(&[(&wrapped, advance_bb)]);
    ctx.builder
        .build_unconditional_branch(probe_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    Ok(ReadOnlyProbe {
        advance_bb,
        e_ptr,
        found_bb,
        not_found_bb,
        pidx,
        pidx_phi,
        s_ptr,
    })
}

pub(crate) fn emit_has_q<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i1_ty = ctx.context.bool_type();
    let table = extract_table_fields(ctx, function, llvm_function)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &table,
        key_val,
        &key_ops,
    )?;
    let _ = (probe.e_ptr, probe.pidx, probe.s_ptr, probe.pidx_phi);

    ctx.builder.position_at_end(probe.found_bb);
    ret_basic(ctx, function, i1_ty.const_int(1, false).into())?;
    ctx.builder.position_at_end(probe.not_found_bb);
    ret_basic(ctx, function, i1_ty.const_zero().into())
}

pub(crate) fn emit_remove<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    // emit_remove keeps the manual 4-step extract because it needs
    // `self_val` for the not-found return — `extract_table_fields`
    // discards the original struct. Copy-on-write: the found path
    // tombstones a fresh clone of the buffers, so the receiver's
    // table is never mutated in place through a shared binding.
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let original = TableSnapshot {
        entries_ptr: extract_pointer(ctx, function, self_val, 0, "entries")?,
        states_ptr: extract_pointer(ctx, function, self_val, 1, "states")?,
        length: extract_int(ctx, function, self_val, 2, "len")?,
        capacity: extract_int(ctx, function, self_val, 3, "cap")?,
    };
    let table = clone_table_buffers(ctx, function, &original, layout.entry_size)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &table,
        key_val,
        &key_ops,
    )?;
    let _ = (probe.e_ptr, probe.pidx, probe.pidx_phi, probe.advance_bb);

    ctx.builder.position_at_end(probe.found_bb);
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_TOMBSTONE, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_sub(table.length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let removed = build_table_struct(
        ctx,
        function,
        table.entries_ptr,
        table.states_ptr,
        new_len,
        table.capacity,
    )?;
    ret_struct(ctx, function, removed)?;

    ctx.builder.position_at_end(probe.not_found_bb);
    ret_struct(ctx, function, self_val)
}

pub(crate) fn emit_map_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let value_ty = layout.value_ty.ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Map.get layout missing value type (symbol `{}`)",
            function.symbol,
        ))
    })?;
    let option_symbol = expect_enum_symbol(&function.return_type, function, "Map.get")?;
    let value_basic_ty = ir_basic_type(ctx, value_ty)?;

    let table = extract_table_fields(ctx, function, llvm_function)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &table,
        key_val,
        &key_ops,
    )?;
    let _ = (probe.pidx, probe.pidx_phi, probe.advance_bb, probe.s_ptr);

    ctx.builder.position_at_end(probe.found_bb);
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
    let val = ctx
        .builder
        .build_load(value_basic_ty, val_ptr, "val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[val])?;
    ctx.builder
        .build_return(Some(&some))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(probe.not_found_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder
        .build_return(Some(&none))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))
}
