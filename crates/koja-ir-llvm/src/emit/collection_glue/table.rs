//! `Map<K,V>` / `Set<T>` clone / drop glue: the open-addressed
//! hashtable bucket walk. Layout is `{ entries_ptr, states_ptr, len,
//! cap }` (see [`crate::types::hashtable_value_type`]); `entries_ptr`
//! is a flat `[Entry; cap]` and `states_ptr` a `[u8; cap]` occupancy
//! map. `Set`'s entry is a bare `K`; `Map`'s entry is `K` then `V` at
//! byte offset `key_size` — the packed layout the hashtable intrinsics
//! write.

use inkwell::IntPredicate;
use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::STATE_OCCUPIED;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::hashtable_value_type;

use super::{
    abi_size, acquire_element, call_ptr, element_slot, extract_int, extract_pointer, nth_struct,
    release_element,
};

/// `clone_Map<K,V>` / `clone_Set<T>`: deep-copy both backing buffers,
/// then acquire the key (and, for `Map`, the value) of every occupied
/// bucket so the copy owns independent references. `value` is `None`
/// for `Set` and `Some(V)` for `Map` (the value sits at byte offset
/// `key_size` within the entry).
pub(super) fn clone_table<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    key: &IRType,
    value: Option<&IRType>,
) -> Result<(), LlvmError> {
    let key_size = abi_size(ctx, key)?;
    let entry_size = key_size + value.map(|v| abi_size(ctx, v)).transpose()?.unwrap_or(0);
    let entry_size_const = ctx.context.i64_type().const_int(entry_size, false);

    let self_val = nth_struct(function, llvm_function, 0)?;
    let entries = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states = extract_pointer(ctx, function, self_val, 1, "states")?;
    let len = extract_int(ctx, function, self_val, 2, "len")?;
    let capacity = extract_int(ctx, function, self_val, 3, "cap")?;

    let entries_bytes = ctx
        .builder
        .build_int_mul(capacity, entry_size_const, "entries_bytes")
        .map_err(|e| inkwell_err(format_args!("clone_table mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let dst_entries = call_ptr(
        ctx,
        function,
        malloc,
        &[entries_bytes.into()],
        "dst_entries",
    )?;
    let dst_states = call_ptr(ctx, function, malloc, &[capacity.into()], "dst_states")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[dst_entries.into(), entries.into(), entries_bytes.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("clone_table entries memcpy for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_call(
            memcpy,
            &[dst_states.into(), states.into(), capacity.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("clone_table states memcpy for `{}`", function.symbol),
                e,
            )
        })?;

    emit_occupied_loop(
        ctx,
        llvm_function,
        dst_states,
        capacity,
        "clone",
        |ctx, index| {
            let entry_ptr = element_slot(ctx, function, dst_entries, index, entry_size_const)?;
            acquire_element(ctx, function, key, entry_ptr)?;
            if let Some(value_ty) = value {
                let value_ptr = offset_ptr(ctx, function, entry_ptr, key_size, "value_ptr")?;
                acquire_element(ctx, function, value_ty, value_ptr)?;
            }
            Ok(())
        },
    )?;

    let result = build_table_struct(ctx, function, dst_entries, dst_states, len, capacity)?;
    ctx.builder
        .build_return(Some(&result))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("clone_table ret for `{}`", function.symbol), e))
}

/// `drop_Map<K,V>` / `drop_Set<T>`: release the key (and value) of
/// every occupied bucket, then free both backing buffers.
pub(super) fn drop_table<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    key: &IRType,
    value: Option<&IRType>,
) -> Result<(), LlvmError> {
    let key_size = abi_size(ctx, key)?;
    let entry_size = key_size + value.map(|v| abi_size(ctx, v)).transpose()?.unwrap_or(0);
    let entry_size_const = ctx.context.i64_type().const_int(entry_size, false);

    let self_val = nth_struct(function, llvm_function, 0)?;
    let entries = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states = extract_pointer(ctx, function, self_val, 1, "states")?;
    let capacity = extract_int(ctx, function, self_val, 3, "cap")?;

    emit_occupied_loop(
        ctx,
        llvm_function,
        states,
        capacity,
        "drop",
        |ctx, index| {
            let entry_ptr = element_slot(ctx, function, entries, index, entry_size_const)?;
            release_element(ctx, function, key, entry_ptr)?;
            if let Some(value_ty) = value {
                let value_ptr = offset_ptr(ctx, function, entry_ptr, key_size, "value_ptr")?;
                release_element(ctx, function, value_ty, value_ptr)?;
            }
            Ok(())
        },
    )?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[entries.into()], "")
        .map_err(|e| {
            inkwell_err(
                format_args!("drop_table entries free for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_call(free, &[states.into()], "")
        .map_err(|e| {
            inkwell_err(
                format_args!("drop_table states free for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("drop_table ret for `{}`", function.symbol), e))
}

/// Emit a `for index in 0..capacity { if states[index] == OCCUPIED {
/// body } }` walk over a hashtable's buckets. Like the list helper the
/// straight-line `body` runs once into the per-occupied-bucket block;
/// the helper owns the counter, the range guard, the occupancy branch,
/// and the back-edge.
fn emit_occupied_loop<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    states: PointerValue<'ctx>,
    capacity: IntValue<'ctx>,
    label: &str,
    body: impl FnOnce(&EmitContext<'ctx>, IntValue<'ctx>) -> Result<(), LlvmError>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let counter = ctx.build_entry_alloca(i64_ty, &format!("{label}.i"));
    ctx.builder
        .build_store(counter, i64_ty.const_zero())
        .map_err(|e| inkwell_err(format_args!("{label} loop counter init"), e))?;
    let head = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.head"));
    let check = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.check"));
    let occupied = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.occupied"));
    let next = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.next"));
    let exit = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.exit"));

    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop entry branch"), e))?;
    ctx.builder.position_at_end(head);
    let index = ctx
        .builder
        .build_load(i64_ty, counter, &format!("{label}.idx"))
        .map_err(|e| inkwell_err(format_args!("{label} loop index load"), e))?
        .into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, capacity, &format!("{label}.cmp"))
        .map_err(|e| inkwell_err(format_args!("{label} loop guard"), e))?;
    ctx.builder
        .build_conditional_branch(in_range, check, exit)
        .map_err(|e| inkwell_err(format_args!("{label} loop branch"), e))?;

    ctx.builder.position_at_end(check);
    let state_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, states, &[index], &format!("{label}.state_ptr"))
            .map_err(|e| inkwell_err(format_args!("{label} state GEP"), e))?
    };
    let state = ctx
        .builder
        .build_load(i8_ty, state_ptr, &format!("{label}.state"))
        .map_err(|e| inkwell_err(format_args!("{label} state load"), e))?
        .into_int_value();
    let is_occupied = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            state,
            i8_ty.const_int(STATE_OCCUPIED, false),
            &format!("{label}.is_occupied"),
        )
        .map_err(|e| inkwell_err(format_args!("{label} occupancy compare"), e))?;
    ctx.builder
        .build_conditional_branch(is_occupied, occupied, next)
        .map_err(|e| inkwell_err(format_args!("{label} occupancy branch"), e))?;

    ctx.builder.position_at_end(occupied);
    body(ctx, index)?;
    ctx.builder
        .build_unconditional_branch(next)
        .map_err(|e| inkwell_err(format_args!("{label} occupied-to-next branch"), e))?;

    ctx.builder.position_at_end(next);
    let incremented = ctx
        .builder
        .build_int_add(index, i64_ty.const_int(1, false), &format!("{label}.inc"))
        .map_err(|e| inkwell_err(format_args!("{label} loop increment"), e))?;
    ctx.builder
        .build_store(counter, incremented)
        .map_err(|e| inkwell_err(format_args!("{label} loop counter store"), e))?;
    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop back-edge"), e))?;

    ctx.builder.position_at_end(exit);
    Ok(())
}

/// Pointer `bytes` past `base` — the in-entry value slot of a `Map`
/// bucket (`base` is the key at offset 0, the value sits at
/// `key_size`).
fn offset_ptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    base: PointerValue<'ctx>,
    bytes: u64,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let offset = ctx.context.i64_type().const_int(bytes, false);
    unsafe {
        ctx.builder
            .build_gep(i8_ty, base, &[offset], name)
            .map_err(|e| inkwell_err(format_args!("offset GEP for `{}`", function.symbol), e))
    }
}

fn build_table_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    entries: PointerValue<'ctx>,
    states: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let table_ty = hashtable_value_type(ctx);
    let with_entries = ctx
        .builder
        .build_insert_value(table_ty.get_undef(), entries, 0, "with_entries")
        .map_err(|e| {
            inkwell_err(
                format_args!("table insert entries for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let with_states = ctx
        .builder
        .build_insert_value(with_entries, states, 1, "with_states")
        .map_err(|e| {
            inkwell_err(
                format_args!("table insert states for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let with_len = ctx
        .builder
        .build_insert_value(with_states, len, 2, "with_len")
        .map_err(|e| {
            inkwell_err(
                format_args!("table insert len for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    ctx.builder
        .build_insert_value(with_len, cap, 3, "with_cap")
        .map(|s| s.into_struct_value())
        .map_err(|e| {
            inkwell_err(
                format_args!("table insert cap for `{}`", function.symbol),
                e,
            )
        })
}
