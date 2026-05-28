//! Load-factor check + rehash machinery used by every write path.
//! Both `Map.put` and `Set.insert` call [`emit_resize_if_needed`]
//! before probing for a free slot — if the table is at 3/4 occupancy
//! it grows by 2x and rehashes; otherwise the originals pass through
//! the phi join unchanged.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_free_extern, declare_malloc_extern, declare_memset_extern};

use super::util::{
    KeyHashOps, TableSnapshot, advance_slot, call_hash, call_malloc, codegen_err, entry_pointer,
};
use super::{HashtableLayout, STATE_EMPTY, STATE_OCCUPIED};

/// Emit the load-factor check, the resize-and-rehash path, and the
/// resize-or-not phi join. Returns the live table snapshot for the
/// probe block to consume — same `length` as the input (no insert
/// has happened yet), but possibly swapped buffers and grown
/// `capacity`. Builder ends positioned at the post-join block.
pub(super) fn emit_resize_if_needed<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    table: &TableSnapshot<'ctx>,
    key_ops: &KeyHashOps<'ctx>,
) -> Result<TableSnapshot<'ctx>, LlvmError> {
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let ptr_ty = ctx.context.ptr_type(inkwell::AddressSpace::default());

    let need_resize_bb = ctx.context.append_basic_block(llvm_function, "need_resize");
    let no_resize_bb = ctx.context.append_basic_block(llvm_function, "no_resize");
    let after_resize_bb = ctx
        .context
        .append_basic_block(llvm_function, "after_resize");

    let len_plus_1 = ctx
        .builder
        .build_int_add(table.length, i64_ty.const_int(1, false), "len_plus_1")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let lhs = ctx
        .builder
        .build_int_mul(len_plus_1, i64_ty.const_int(4, false), "lhs")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let rhs = ctx
        .builder
        .build_int_mul(table.capacity, i64_ty.const_int(3, false), "rhs")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let should_resize = ctx
        .builder
        .build_int_compare(IntPredicate::UGT, lhs, rhs, "should_resize")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(should_resize, need_resize_bb, no_resize_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(need_resize_bb);
    let new_cap = ctx
        .builder
        .build_int_mul(table.capacity, i64_ty.const_int(2, false), "new_cap")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let new_entries_bytes = ctx
        .builder
        .build_int_mul(
            new_cap,
            i64_ty.const_int(layout.entry_size, false),
            "new_e_bytes",
        )
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let new_entries_ptr = call_malloc(ctx, function, malloc, new_entries_bytes, "new_entries")?;
    let new_states_ptr = call_malloc(ctx, function, malloc, new_cap, "new_states")?;
    let memset = declare_memset_extern(ctx);
    ctx.builder
        .build_call(
            memset,
            &[
                new_states_ptr.into(),
                i32_ty.const_zero().into(),
                new_cap.into(),
            ],
            "",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_call memset for `{}`", function.symbol),
                e,
            )
        })?;

    let new_table = TableSnapshot {
        entries_ptr: new_entries_ptr,
        states_ptr: new_states_ptr,
        length: table.length,
        capacity: new_cap,
    };
    emit_rehash_loop(
        ctx,
        function,
        llvm_function,
        layout,
        table,
        &new_table,
        key_ops,
    )?;

    // After rehash, free old buffers and branch to the join.
    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[table.entries_ptr.into()], "")
        .map_err(|e| codegen_err(format_args!("build_call free for `{}`", function.symbol), e))?;
    ctx.builder
        .build_call(free, &[table.states_ptr.into()], "")
        .map_err(|e| codegen_err(format_args!("build_call free for `{}`", function.symbol), e))?;
    let from_resize_bb = ctx.builder.get_insert_block().unwrap();
    ctx.builder
        .build_unconditional_branch(after_resize_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(no_resize_bb);
    ctx.builder
        .build_unconditional_branch(after_resize_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(after_resize_bb);
    let eptr_phi = ctx
        .builder
        .build_phi(ptr_ty, "eptr_phi")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    eptr_phi.add_incoming(&[
        (&new_entries_ptr, from_resize_bb),
        (&table.entries_ptr, no_resize_bb),
    ]);
    let sptr_phi = ctx
        .builder
        .build_phi(ptr_ty, "sptr_phi")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    sptr_phi.add_incoming(&[
        (&new_states_ptr, from_resize_bb),
        (&table.states_ptr, no_resize_bb),
    ]);
    let cap_phi = ctx
        .builder
        .build_phi(i64_ty, "cap_phi")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    cap_phi.add_incoming(&[(&new_cap, from_resize_bb), (&table.capacity, no_resize_bb)]);

    Ok(TableSnapshot {
        entries_ptr: eptr_phi.as_basic_value().into_pointer_value(),
        states_ptr: sptr_phi.as_basic_value().into_pointer_value(),
        length: table.length,
        capacity: cap_phi.as_basic_value().into_int_value(),
    })
}

/// Rehash loop: for each `ri` in `0..old.capacity`, if the old
/// state is OCCUPIED, hash the old key, linear-probe in the new
/// buffer, memcpy the entry, mark new state OCCUPIED. Reads
/// `key_ops.hash_fn` + `key_ops.key_basic_ty` — `eq_fn` is never
/// consulted because moving an already-bucketed key into a larger
/// buffer can't collide with itself. Builder ends positioned at
/// the rehash-done block (caller's next emission continues from
/// there).
fn emit_rehash_loop<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    old: &TableSnapshot<'ctx>,
    new: &TableSnapshot<'ctx>,
    key_ops: &KeyHashOps<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().unwrap();

    let rehash_bb = ctx.context.append_basic_block(llvm_function, "rehash");
    let rehash_body = ctx.context.append_basic_block(llvm_function, "rehash_body");
    let rehash_probe = ctx
        .context
        .append_basic_block(llvm_function, "rehash_probe");
    let rehash_probe_loop = ctx
        .context
        .append_basic_block(llvm_function, "rehash_probe_loop");
    let rehash_advance = ctx
        .context
        .append_basic_block(llvm_function, "rehash_advance");
    let rehash_store = ctx
        .context
        .append_basic_block(llvm_function, "rehash_store");
    let rehash_next = ctx.context.append_basic_block(llvm_function, "rehash_next");
    let rehash_done = ctx.context.append_basic_block(llvm_function, "rehash_done");

    ctx.builder
        .build_unconditional_branch(rehash_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;
    ctx.builder.position_at_end(rehash_bb);
    let ri_phi = ctx
        .builder
        .build_phi(i64_ty, "ri")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    ri_phi.add_incoming(&[(&i64_ty.const_zero(), entry_block)]);
    let ri = ri_phi.as_basic_value().into_int_value();
    let ri_done = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, ri, old.capacity, "ri_done")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(ri_done, rehash_done, rehash_body)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_body);
    let state_at_ri_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, old.states_ptr, &[ri], "old_state_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let state_at_ri = ctx
        .builder
        .build_load(i8_ty, state_at_ri_ptr, "old_state")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let is_occupied = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            state_at_ri,
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
        .build_conditional_branch(is_occupied, rehash_probe, rehash_next)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_probe);
    let old_entry_ptr = entry_pointer(ctx, function, old.entries_ptr, ri, layout.entry_size)?;
    let old_key = ctx
        .builder
        .build_load(key_ops.key_basic_ty, old_entry_ptr, "old_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let old_hash = call_hash(ctx, function, key_ops.hash_fn, old_key)?;
    let new_mask = ctx
        .builder
        .build_int_sub(new.capacity, i64_ty.const_int(1, false), "new_mask")
        .map_err(|e| codegen_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let new_slot_init = ctx
        .builder
        .build_and(old_hash, new_mask, "new_slot_init")
        .map_err(|e| codegen_err(format_args!("build_and for `{}`", function.symbol), e))?;
    ctx.builder
        .build_unconditional_branch(rehash_probe_loop)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_probe_loop);
    let rp_slot_phi = ctx
        .builder
        .build_phi(i64_ty, "rp_slot")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    rp_slot_phi.add_incoming(&[(&new_slot_init, rehash_probe)]);
    let rp_slot = rp_slot_phi.as_basic_value().into_int_value();
    let new_state_at = unsafe {
        ctx.builder
            .build_gep(i8_ty, new.states_ptr, &[rp_slot], "ns_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let ns_val = ctx
        .builder
        .build_load(i8_ty, new_state_at, "ns_val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let ns_empty = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            ns_val,
            i8_ty.const_int(STATE_EMPTY, false),
            "ns_empty",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(ns_empty, rehash_store, rehash_advance)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_advance);
    let rp_wrapped = advance_slot(ctx, function, rp_slot, new_mask)?;
    rp_slot_phi.add_incoming(&[(&rp_wrapped, rehash_advance)]);
    ctx.builder
        .build_unconditional_branch(rehash_probe_loop)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_store);
    let new_entry_ptr = entry_pointer(ctx, function, new.entries_ptr, rp_slot, layout.entry_size)?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[
                new_entry_ptr.into(),
                old_entry_ptr.into(),
                i64_ty.const_int(layout.entry_size, false).into(),
            ],
            "",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_call memcpy for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_store(new_state_at, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_unconditional_branch(rehash_next)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_next);
    let ri_next = ctx
        .builder
        .build_int_add(ri, i64_ty.const_int(1, false), "ri_next")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    ri_phi.add_incoming(&[(&ri_next, rehash_next)]);
    ctx.builder
        .build_unconditional_branch(rehash_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(rehash_done);
    Ok(())
}
