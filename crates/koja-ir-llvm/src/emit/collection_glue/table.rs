//! `Map<K,V>` / `Set<T>` clone / drop glue: the open-addressed
//! hashtable bucket walk. Layout is `{ entries_ptr, states_ptr, len,
//! cap }` (see [`crate::types::hashtable_value_type`]); `entries_ptr`
//! is a flat `[Entry; cap]` and `states_ptr` a `[u8; cap]` occupancy
//! map. `Set`'s entry is a bare `K`; `Map`'s entry is `K` then `V` at
//! byte offset `key_size` — the packed layout the hashtable intrinsics
//! write.

use inkwell::values::{FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::{acquire_in_slot, element_slot, release_in_slot};
use crate::intrinsics::occupied_loop;
use crate::runtime::{declare_free_extern, declare_malloc_extern};
use crate::types::hashtable_value_type;

use super::{abi_size, call_ptr, extract_int, extract_pointer, nth_struct};

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

    occupied_loop(
        ctx,
        llvm_function,
        dst_states,
        capacity,
        "clone",
        |ctx, index| {
            let entry_ptr = element_slot(ctx, function, dst_entries, index, entry_size_const)?;
            acquire_in_slot(ctx, function, key, entry_ptr)?;
            if let Some(value_ty) = value {
                let value_ptr = offset_ptr(ctx, function, entry_ptr, key_size, "value_ptr")?;
                acquire_in_slot(ctx, function, value_ty, value_ptr)?;
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

    occupied_loop(
        ctx,
        llvm_function,
        states,
        capacity,
        "drop",
        |ctx, index| {
            let entry_ptr = element_slot(ctx, function, entries, index, entry_size_const)?;
            release_in_slot(ctx, function, key, entry_ptr)?;
            if let Some(value_ty) = value {
                let value_ptr = offset_ptr(ctx, function, entry_ptr, key_size, "value_ptr")?;
                release_in_slot(ctx, function, value_ty, value_ptr)?;
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
