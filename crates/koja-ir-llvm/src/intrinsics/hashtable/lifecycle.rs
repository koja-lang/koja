//! Allocate / inspect emitters: `new`, `length`, `empty?`, and the
//! `from_map` identity. None of these touch the probe loop — they
//! either mint a fresh buffer pair or peek at the table fields.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::runtime::{declare_malloc_extern, declare_memset_extern};

use super::INITIAL_CAPACITY;
use super::util::{
    build_table_struct, call_malloc, codegen_err, extract_int, nth_hashtable, ret_basic, ret_struct,
};

/// `fn new() -> Self` — allocate the entries + states buffers and
/// initialize state to `EMPTY`. Same shape for `Map.new` and
/// `Set.new`; the only knob is `entry_size`.
pub(crate) fn emit_new<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    entry_size: u64,
) -> Result<(), LlvmError> {
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let capacity = i64_ty.const_int(INITIAL_CAPACITY, false);
    let entry_size_const = i64_ty.const_int(entry_size, false);

    let entries_bytes = ctx
        .builder
        .build_int_mul(capacity, entry_size_const, "entries_bytes")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let entries_ptr = call_malloc(ctx, function, malloc, entries_bytes, "entries")?;
    let states_ptr = call_malloc(ctx, function, malloc, capacity, "states")?;

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
        .map_err(|e| {
            codegen_err(
                format_args!("build_call memset for `{}`", function.symbol),
                e,
            )
        })?;

    let result = build_table_struct(
        ctx,
        function,
        entries_ptr,
        states_ptr,
        i64_ty.const_zero(),
        capacity,
    )?;
    ret_struct(ctx, function, result)
}

/// `fn length(self) -> Int` — return the `length` field. Both
/// collections.
pub(crate) fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let len = extract_int(ctx, function, self_val, 2, "len")?;
    ret_basic(ctx, function, len.into())
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
    let len = extract_int(ctx, function, self_val, 2, "len")?;
    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ret_basic(ctx, function, is_empty.into())
}

/// Identity-shaped intrinsics: `Map.from_map(self) -> Self`.
/// Returns the receiver unchanged.
pub(crate) fn emit_identity<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    ret_struct(ctx, function, self_val)
}
