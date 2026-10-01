//! `Map<K,V>` / `Set<T>` clone / drop glue, the open-addressed
//! hashtable bucket walk. Layout is `{ entries_ptr, states_ptr, len,
//! cap }` (see [`crate::types::hashtable_value_type`]), where
//! `entries_ptr` is a flat `[Entry; cap]` and `states_ptr` a
//! `[u8; cap]` occupancy map. `Set`'s entry is a bare `K`. `Map`'s
//! entry is `K` then `V` at byte offset `key_size`: the packed
//! layout the hashtable intrinsics write.

use inkwell::values::FunctionValue;
use koja_ir::{IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::ElementOp;
use crate::intrinsics::util::build_table_struct;
use crate::intrinsics::{
    HashtableLayout, TableSnapshot, apply_occupied, clone_table, extract_table_fields,
};
use crate::runtime::declare_free_extern;
use crate::types::abi_size;

/// `clone_Map<K,V>` / `clone_Set<T>` and their `deep_copy_*`
/// siblings copy both backing buffers, then apply `op` to the key
/// (and, for `Map`, the value) of every occupied bucket so the copy
/// owns independent references. `value` is `None` for `Set` and
/// `Some(V)` for `Map` (the value sits at byte offset `key_size`
/// within the entry).
pub(super) fn copy_table<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    key: &IRType,
    value: Option<&IRType>,
    op: ElementOp,
) -> Result<(), LlvmError> {
    let (src, layout) = table_prologue(ctx, function, llvm_function, key, value)?;
    let dst = clone_table(ctx, llvm_function, &layout, &src, op)?;
    let result = build_table_struct(
        ctx,
        dst.entries_ptr,
        dst.states_ptr,
        dst.length,
        dst.capacity,
    )?;
    ctx.builder.build_return(Some(&result)).or_ice().map(|_| ())
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
    let (table, layout) = table_prologue(ctx, function, llvm_function, key, value)?;
    apply_occupied(
        ctx,
        llvm_function,
        &layout,
        &table,
        ElementOp::Release,
        "drop",
    )?;

    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[table.entries_ptr.into()], "")
        .or_ice()?;
    ctx.builder
        .build_call(free, &[table.states_ptr.into()], "")
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// Read `self` and size the entry for the `key` / `value` pair the
/// glue was instantiated at. Shared by [`copy_table`] and
/// [`drop_table`].
fn table_prologue<'ctx, 'ty>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    key: &'ty IRType,
    value: Option<&'ty IRType>,
) -> Result<(TableSnapshot<'ctx>, HashtableLayout<'ty>), LlvmError> {
    let key_size = abi_size(ctx, key)?;
    let value_size = value.map(|v| abi_size(ctx, v)).transpose()?.unwrap_or(0);
    let layout = HashtableLayout {
        entry_size: key_size + value_size,
        key_size,
        key_ty: key,
        value_ty: value,
    };
    let table = extract_table_fields(ctx, function, llvm_function)?;
    Ok((table, layout))
}
