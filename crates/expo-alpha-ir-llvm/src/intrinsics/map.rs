//! `Map<K, V>` family — heap-backed open-addressed hash table.
//! Layout matches [`crate::types::hashtable_value_type`]:
//! `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }`.
//! Entries live off-heap behind `entries_ptr` as a flat
//! `[Entry; capacity]` where each `Entry` is a `(K, V)` pair laid out
//! by ABI; `states_ptr` is `[u8; capacity]` (`0` empty / `1`
//! occupied / `2` tombstone). Both buffers malloc on `new`, realloc
//! on resize, and free on drop.
//!
//! Mirrors v1 [`expo_codegen::map`] one-to-one, ported to the alpha
//! emit context.

use expo_alpha_ir::{IRFunction, IRType, MapMethod};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::intrinsics::hashtable;
use inkwell::values::FunctionValue;

pub(super) fn emit_map<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: MapMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let (key, value) = key_value(method, function)?;
    let key_size = hashtable::ir_byte_size(ctx, key)?;
    let value_size = hashtable::ir_byte_size(ctx, value)?;
    let entry_size = key_size + value_size;
    let layout = hashtable::HashtableLayout {
        entry_size,
        key_size,
        key_ty: key,
        value_ty: Some(value),
    };

    match method {
        MapMethod::EmptyQ => hashtable::emit_empty_q(ctx, function, llvm_function),
        MapMethod::FromMap => hashtable::emit_identity(ctx, function, llvm_function),
        MapMethod::Get => hashtable::emit_map_get(ctx, function, llvm_function, &layout),
        MapMethod::HasQ => hashtable::emit_has_q(ctx, function, llvm_function, &layout),
        MapMethod::Length => hashtable::emit_length(ctx, function, llvm_function),
        MapMethod::New => hashtable::emit_new(ctx, function, entry_size),
        MapMethod::Put => hashtable::emit_map_put(ctx, function, llvm_function, &layout),
        MapMethod::Remove => hashtable::emit_remove(ctx, function, llvm_function, &layout),
    }
}

/// Resolve `(K, V)` for a `Map<K, V>` intrinsic. `new` carries them
/// on the return type; every other method has `self: Map<K, V>` as
/// `params[0]`.
fn key_value(method: MapMethod, function: &IRFunction) -> Result<(&IRType, &IRType), LlvmError> {
    let candidate = match method {
        MapMethod::New => &function.return_type,
        _ => &function.params[0].ty,
    };
    match candidate {
        IRType::Map { key, value } => Ok((key, value)),
        other => Err(LlvmError::Codegen(format!(
            "Map.{method:?} expected a `Map<K, V>` slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}
