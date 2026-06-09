//! Allocate / inspect emitters: `new`, `length`, `empty?`, and the
//! `from_map` identity. None of these touch the probe loop — they
//! either mint a fresh buffer pair or peek at the table fields.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_malloc_extern, declare_memset_extern};

use super::util::{
    TableSnapshot, build_table_struct, call_malloc, clone_table_buffers, extract_int,
    extract_pointer, nth_hashtable, ret_basic, ret_struct,
};
use super::{HashtableLayout, INITIAL_CAPACITY};

/// `fn new() -> Self` — allocate the entries + states buffers and
/// initialize state to `EMPTY`. Same shape for `Map.new` and
/// `Set.new`; the only knob is `entry_size`.
pub(crate) fn emit_new<'ctx>(ctx: &EmitContext<'ctx>, entry_size: u64) -> Result<(), LlvmError> {
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let capacity = i64_ty.const_int(INITIAL_CAPACITY, false);
    let entry_size_const = i64_ty.const_int(entry_size, false);

    let entries_bytes = ctx
        .builder
        .build_int_mul(capacity, entry_size_const, "entries_bytes")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let entries_ptr = call_malloc(ctx, malloc, entries_bytes, "entries")?;
    let states_ptr = call_malloc(ctx, malloc, capacity, "states")?;

    let memset = declare_memset_extern(ctx);
    ctx.builder
        .build_call(
            memset,
            &[
                states_ptr.into(),
                i32_ty.const_zero().into(),
                capacity.into(),
            ],
            "",
        )
        .or_ice()?;

    let result = build_table_struct(ctx, entries_ptr, states_ptr, i64_ty.const_zero(), capacity)?;
    ret_struct(ctx, result)
}

/// `fn length(self) -> Int` — return the `length` field. Both
/// collections.
pub(crate) fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let len = extract_int(ctx, self_val, 2, "len")?;
    ret_basic(ctx, len.into())
}

/// `fn empty?(self) -> Bool` — check `length == 0`. Both
/// collections.
pub(crate) fn emit_empty_q<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let len = extract_int(ctx, self_val, 2, "len")?;
    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .or_ice()?;
    ret_basic(ctx, is_empty.into())
}

/// Identity-shaped intrinsics: `Map.from_map(self) -> Self` and
/// `Set.from_set(self) -> Self`. Value-wise an identity, but `self` is
/// borrowed and dropped by the caller, so the result must own
/// independent buffers — clone rather than alias `self`'s.
pub(crate) fn emit_identity<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let original = TableSnapshot {
        entries_ptr: extract_pointer(ctx, self_val, 0, "entries")?,
        states_ptr: extract_pointer(ctx, self_val, 1, "states")?,
        length: extract_int(ctx, self_val, 2, "len")?,
        capacity: extract_int(ctx, self_val, 3, "cap")?,
    };
    let cloned = clone_table_buffers(ctx, llvm_function, layout, &original)?;
    let result = build_table_struct(
        ctx,
        cloned.entries_ptr,
        cloned.states_ptr,
        cloned.length,
        cloned.capacity,
    )?;
    ret_struct(ctx, result)
}
