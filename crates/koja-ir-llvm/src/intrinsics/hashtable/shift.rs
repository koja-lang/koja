//! The backward shift that `remove` runs after it frees a slot.
//!
//! A probe stops at the first EMPTY slot. A slot freed in the middle
//! of a run of entries would hide every entry after it, so `remove`
//! moves those entries back into the hole, as Swift's `_HashTable`
//! does. A table then needs no third "deleted" state, and the load
//! check in [`super::resize`] can count live entries only.

use inkwell::IntPredicate;
use inkwell::values::{FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::declare_memcpy_extern;

use super::util::{KeyHashOps, TableSnapshot, advance_slot, call_hash, entry_pointer};
use super::{HashtableLayout, STATE_EMPTY};

/// Close the hole that `remove` left at `hole_slot`. The caller has
/// released the entry in that slot and has not changed its state.
///
/// The loop walks the slots after the hole until the first EMPTY one.
/// An entry moves back into the hole when the hole lies between its
/// home slot and its own slot, because a probe for that entry passes
/// the hole. The slot it leaves becomes the new hole. The last hole
/// becomes EMPTY. Builder ends positioned after that store.
pub(super) fn emit_shift_back<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    table: &TableSnapshot<'ctx>,
    key_ops: &KeyHashOps<'ctx>,
    hole_slot: IntValue<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx
        .builder
        .get_insert_block()
        .expect("emit_shift_back called with no insertion block");
    let mask = ctx
        .builder
        .build_int_sub(table.capacity, i64_ty.const_int(1, false), "shift_mask")
        .or_ice()?;

    let scan_bb = ctx.context.append_basic_block(llvm_function, "shift_scan");
    let check_bb = ctx.context.append_basic_block(llvm_function, "shift_check");
    let move_bb = ctx.context.append_basic_block(llvm_function, "shift_move");
    let done_bb = ctx.context.append_basic_block(llvm_function, "shift_done");

    ctx.builder.build_unconditional_branch(scan_bb).or_ice()?;
    ctx.builder.position_at_end(scan_bb);
    let hole_phi = ctx.builder.build_phi(i64_ty, "hole").or_ice()?;
    hole_phi.add_incoming(&[(&hole_slot, entry_block)]);
    let hole = hole_phi.as_basic_value().into_int_value();
    let scanned_phi = ctx.builder.build_phi(i64_ty, "scanned").or_ice()?;
    scanned_phi.add_incoming(&[(&hole_slot, entry_block)]);
    let scanned = scanned_phi.as_basic_value().into_int_value();
    let slot = advance_slot(ctx, scanned, mask)?;
    // SAFETY: `slot` is masked to `capacity - 1`, so the GEP stays
    // inside the `states` buffer.
    let state_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, table.states_ptr, &[slot], "shift_state_ptr")
            .or_ice()?
    };
    let state = ctx
        .builder
        .build_load(i8_ty, state_ptr, "shift_state")
        .or_ice()?
        .into_int_value();
    let run_ends = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            state,
            i8_ty.const_int(STATE_EMPTY, false),
            "run_ends",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(run_ends, done_bb, check_bb)
        .or_ice()?;

    // Both distances count slots back from `slot`, modulo the
    // capacity. The entry can move when its home slot is at least as
    // far back as the hole.
    ctx.builder.position_at_end(check_bb);
    let entry_ptr = entry_pointer(ctx, table.entries_ptr, slot, layout.entry_size)?;
    let key = ctx
        .builder
        .build_load(key_ops.key_basic_ty, entry_ptr, "shift_key")
        .or_ice()?;
    let hash = call_hash(ctx, key_ops.hash_fn, key)?;
    let home_slot = ctx.builder.build_and(hash, mask, "home_slot").or_ice()?;
    let to_home = distance_back(ctx, slot, home_slot, mask, "to_home")?;
    let to_hole = distance_back(ctx, slot, hole, mask, "to_hole")?;
    let can_move = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, to_home, to_hole, "can_move")
        .or_ice()?;
    hole_phi.add_incoming(&[(&hole, check_bb)]);
    scanned_phi.add_incoming(&[(&slot, check_bb)]);
    ctx.builder
        .build_conditional_branch(can_move, move_bb, scan_bb)
        .or_ice()?;

    // The move carries the references that the entry owns, so it
    // needs no acquire and no release.
    ctx.builder.position_at_end(move_bb);
    let hole_ptr = entry_pointer(ctx, table.entries_ptr, hole, layout.entry_size)?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[
                hole_ptr.into(),
                entry_ptr.into(),
                i64_ty.const_int(layout.entry_size, false).into(),
            ],
            "",
        )
        .or_ice()?;
    hole_phi.add_incoming(&[(&slot, move_bb)]);
    scanned_phi.add_incoming(&[(&slot, move_bb)]);
    ctx.builder.build_unconditional_branch(scan_bb).or_ice()?;

    ctx.builder.position_at_end(done_bb);
    // SAFETY: `hole` is the slot of the removed entry or a slot that
    // the scan read, so the GEP stays inside the `states` buffer.
    let hole_state_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, table.states_ptr, &[hole], "hole_state_ptr")
            .or_ice()?
    };
    ctx.builder
        .build_store(hole_state_ptr, i8_ty.const_int(STATE_EMPTY, false))
        .or_ice()?;
    Ok(())
}

/// Count of slots from `to` forward to `from`, modulo the capacity.
fn distance_back<'ctx>(
    ctx: &EmitContext<'ctx>,
    from: IntValue<'ctx>,
    to: IntValue<'ctx>,
    mask: IntValue<'ctx>,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let difference = ctx.builder.build_int_sub(from, to, "difference").or_ice()?;
    ctx.builder.build_and(difference, mask, name).or_ice()
}
