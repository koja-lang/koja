//! Allocate / inspect / duplicate emitters: `new`, `length`,
//! `empty?`, `from_map` identity, and the `Map.clone` / `Set.clone`
//! deep copy. None of these touch the probe loop — they either mint
//! a fresh buffer pair, peek at the table fields, or walk every slot
//! to clone its contents.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_malloc_extern, declare_memset_extern};
use crate::types::ir_basic_type;

use super::util::{
    build_table_struct, call_clone, call_malloc, codegen_err, entry_pointer, extract_int,
    extract_table_fields, nth_hashtable, resolve_clone_fn, ret_basic, ret_struct,
};
use super::{HashtableLayout, INITIAL_CAPACITY, STATE_OCCUPIED};

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

/// `fn clone(self) -> Self` for both `Map<K, V>` and `Set<T>` —
/// allocate fresh buffers, `memcpy` the states (just `u8` per slot),
/// then walk every slot and clone `K` (and `V` for `Map`) through
/// the type's mangled `.clone` fn. The new entries buffer holds
/// independently-owned values so the cloned table can be mutated /
/// dropped without aliasing the original.
///
/// For element types whose Clone impl resolves to a value passthrough
/// (Copy primitives whose `fn clone(self) -> Self  self  end` body
/// reads-and-returns), we still emit the call — the function exists
/// and inlines away under the optimizer; the alternative (open-
/// coding a "Copy or not" decision here) duplicates the universal-
/// Clone contract from the synthesizer. See
/// [`resolve_clone_fn`](super::util::resolve_clone_fn) for the
/// receiver-symbol shape per [`koja_ir::IRType`] arm.
pub(crate) fn emit_table_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();

    let src = extract_table_fields(ctx, function, llvm_function)?;

    let entries_bytes = ctx
        .builder
        .build_int_mul(
            src.capacity,
            i64_ty.const_int(layout.entry_size, false),
            "entries_bytes",
        )
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let dst_entries = call_malloc(ctx, function, malloc, entries_bytes, "dst_entries")?;
    let dst_states = call_malloc(ctx, function, malloc, src.capacity, "dst_states")?;

    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[
                dst_states.into(),
                src.states_ptr.into(),
                src.capacity.into(),
            ],
            "",
        )
        .map_err(|e| {
            codegen_err(
                format_args!("build_call memcpy states for `{}`", function.symbol),
                e,
            )
        })?;

    let key_clone_fn = resolve_clone_fn(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;
    let value_clone_fn = match layout.value_ty {
        Some(v_ty) => Some(resolve_clone_fn(ctx, function, v_ty)?),
        None => None,
    };
    let value_basic_ty = match layout.value_ty {
        Some(v_ty) => Some(ir_basic_type(ctx, v_ty)?),
        None => None,
    };

    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "emit_table_clone called with no insertion block for `{}`",
            function.symbol,
        ))
    })?;
    let loop_head = ctx.context.append_basic_block(llvm_function, "clone_loop");
    let loop_body = ctx.context.append_basic_block(llvm_function, "clone_body");
    let do_clone = ctx.context.append_basic_block(llvm_function, "do_clone");
    let next = ctx.context.append_basic_block(llvm_function, "clone_next");
    let done = ctx.context.append_basic_block(llvm_function, "clone_done");

    ctx.builder
        .build_unconditional_branch(loop_head)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(loop_head);
    let idx_phi = ctx
        .builder
        .build_phi(i64_ty, "idx")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    idx_phi.add_incoming(&[(&i64_ty.const_zero(), entry_block)]);
    let idx = idx_phi.as_basic_value().into_int_value();
    let idx_done = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, idx, src.capacity, "idx_done")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(idx_done, done, loop_body)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(loop_body);
    let s_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, src.states_ptr, &[idx], "s_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let s_val = ctx
        .builder
        .build_load(i8_ty, s_ptr, "s_val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
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
        .build_conditional_branch(is_occ, do_clone, next)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(do_clone);
    let src_entry_ptr = entry_pointer(ctx, function, src.entries_ptr, idx, layout.entry_size)?;
    let dst_entry_ptr = entry_pointer(ctx, function, dst_entries, idx, layout.entry_size)?;

    let src_key = ctx
        .builder
        .build_load(key_basic_ty, src_entry_ptr, "src_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let dst_key = call_clone(ctx, function, key_clone_fn, src_key, "dst_key")?;
    ctx.builder
        .build_store(dst_entry_ptr, dst_key)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;

    if let (Some(v_basic), Some(v_clone)) = (value_basic_ty, value_clone_fn) {
        let src_v_ptr = unsafe {
            ctx.builder
                .build_gep(
                    i8_ty,
                    src_entry_ptr,
                    &[i64_ty.const_int(layout.key_size, false)],
                    "src_v_ptr",
                )
                .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
        };
        let dst_v_ptr = unsafe {
            ctx.builder
                .build_gep(
                    i8_ty,
                    dst_entry_ptr,
                    &[i64_ty.const_int(layout.key_size, false)],
                    "dst_v_ptr",
                )
                .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
        };
        let src_val = ctx
            .builder
            .build_load(v_basic, src_v_ptr, "src_val")
            .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
        let dst_val = call_clone(ctx, function, v_clone, src_val, "dst_val")?;
        ctx.builder
            .build_store(dst_v_ptr, dst_val)
            .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    }

    ctx.builder
        .build_unconditional_branch(next)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(next);
    let idx_next = ctx
        .builder
        .build_int_add(idx, i64_ty.const_int(1, false), "idx_next")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let next_block = ctx.builder.get_insert_block().unwrap();
    idx_phi.add_incoming(&[(&idx_next, next_block)]);
    ctx.builder
        .build_unconditional_branch(loop_head)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(done);
    let result = build_table_struct(
        ctx,
        function,
        dst_entries,
        dst_states,
        src.length,
        src.capacity,
    )?;
    ret_struct(ctx, function, result)
}
