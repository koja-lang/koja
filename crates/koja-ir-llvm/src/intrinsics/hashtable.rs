//! Shared hashtable infrastructure for `Map<K, V>` and `Set<T>`.
//!
//! Both collections sit on the same open-addressed, linear-probing
//! table layout described in [`crate::types::hashtable_value_type`].
//! `Map`'s entry is a `(K, V)` pair; `Set`'s entry is a single `T`.
//! All the probe / resize / state-machine bookkeeping is identical
//! between them — only the entry size and the optional value-side
//! payload differ. This module owns the shared emitters; the per-
//! collection modules ([`super::map`], [`super::set`]) stitch them
//! into the per-method dispatch.
//!
//! Errors surface as typed [`LlvmError::Codegen`] values.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{
    BasicValueEnum, FunctionValue, IntValue, PhiValue, PointerValue, StructValue,
};
use koja_ir::mangling::{global_primitive_symbol, mangled_method_name};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_free_extern, declare_malloc_extern, declare_memset_extern};
use crate::types::{hashtable_value_type, ir_basic_type, list_value_type};

/// Initial bucket count for a freshly-allocated hashtable. Matches
/// v1's `INITIAL_CAPACITY` so eval / native / future JIT all
/// agree on the first-resize threshold.
const INITIAL_CAPACITY: u64 = 8;

/// Per-byte state of a single bucket. `STATE_EMPTY` is `memset`-
/// produced (`0`) so a fresh malloc + memset routes through one
/// runtime call. `STATE_OCCUPIED` is "live entry, probe match
/// considers it". `STATE_TOMBSTONE` is "deleted entry, probe must
/// advance past it" — used by `remove` to keep the linear-probe
/// chain intact without back-shifting.
const STATE_EMPTY: u64 = 0;
const STATE_OCCUPIED: u64 = 1;
const STATE_TOMBSTONE: u64 = 2;

/// `Option<V>` variant tags as the stdlib declares them: `Some`
/// first (tag 0), then `None`. Map.get mints either flavour through
/// these so the numeric tags don't leak into the emitter body.
const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

/// Per-instantiation layout knob for the per-method emitters. Set
/// passes `value_ty: None` (the entry is just `T`); Map passes
/// `Some(V)` (the entry is `K` followed by `V`).
pub(super) struct HashtableLayout<'ty> {
    pub entry_size: u64,
    pub key_size: u64,
    pub key_ty: &'ty IRType,
    #[allow(dead_code)]
    pub value_ty: Option<&'ty IRType>,
}

// ---------------------------------------------------------------------------
// Shared method emitters: `new`, `length`, `empty?`, identity (`from_map`)
// ---------------------------------------------------------------------------

/// `fn new() -> Self` — allocate the entries + states buffers and
/// initialize state to `EMPTY`. Same shape for `Map.new` and
/// `Set.new`; the only knob is `entry_size`.
pub(super) fn emit_new<'ctx>(
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
pub(super) fn emit_length<'ctx>(
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
pub(super) fn emit_empty_q<'ctx>(
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
pub(super) fn emit_identity<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    ret_struct(ctx, function, self_val)
}

// ---------------------------------------------------------------------------
// Read-only probe used by `get`, `has?`, `remove` (both Map and Set)
// ---------------------------------------------------------------------------

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
/// Builder must be positioned at a predecessor block before calling
/// (typically the entry block); on return the builder sits at an
/// unspecified location and the caller positions itself at the
/// returned `found_bb` / `not_found_bb` blocks to emit the outcome.
/// The `advance` edge wires itself; the caller does **not** need
/// to mutate the returned `pidx_phi` directly (it's exposed so
/// callers can attach extra incoming edges from custom entry-side
/// branching, e.g. `put`'s resize-or-not phi).
#[allow(clippy::too_many_arguments)]
fn emit_read_only_probe<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    entries_ptr: PointerValue<'ctx>,
    states_ptr: PointerValue<'ctx>,
    capacity: IntValue<'ctx>,
    key_val: BasicValueEnum<'ctx>,
    hash_fn: FunctionValue<'ctx>,
    eq_fn: FunctionValue<'ctx>,
    key_basic_ty: BasicTypeEnum<'ctx>,
) -> Result<ReadOnlyProbe<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "emit_read_only_probe called with no insertion block for `{}`",
            function.symbol,
        ))
    })?;

    let hash_val = call_hash(ctx, function, hash_fn, key_val)?;
    let mask = ctx
        .builder
        .build_int_sub(capacity, i64_ty.const_int(1, false), "mask")
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
            .build_gep(i8_ty, states_ptr, &[pidx], "s_ptr")
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
    let e_ptr = entry_pointer(ctx, function, entries_ptr, pidx, layout.entry_size)?;
    let existing_key = ctx
        .builder
        .build_load(key_basic_ty, e_ptr, "existing_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let keys_equal = call_eq(ctx, function, eq_fn, key_val, existing_key)?;
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

// ---------------------------------------------------------------------------
// Map.get / has? / Set.has? — read-only probe with per-method tail
// ---------------------------------------------------------------------------

pub(super) fn emit_has_q<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i1_ty = ctx.context.bool_type();
    let (entries_ptr, states_ptr, _length, capacity) =
        extract_table_fields(ctx, function, llvm_function)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        capacity,
        key_val,
        hash_fn,
        eq_fn,
        key_basic_ty,
    )?;
    let _ = (probe.e_ptr, probe.pidx, probe.s_ptr, probe.pidx_phi);

    ctx.builder.position_at_end(probe.found_bb);
    ret_basic(ctx, function, i1_ty.const_int(1, false).into())?;
    ctx.builder.position_at_end(probe.not_found_bb);
    ret_basic(ctx, function, i1_ty.const_zero().into())
}

pub(super) fn emit_remove<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let entries_ptr = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states_ptr = extract_pointer(ctx, function, self_val, 1, "states")?;
    let length = extract_int(ctx, function, self_val, 2, "len")?;
    let capacity = extract_int(ctx, function, self_val, 3, "cap")?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        capacity,
        key_val,
        hash_fn,
        eq_fn,
        key_basic_ty,
    )?;
    let _ = (probe.e_ptr, probe.pidx, probe.pidx_phi, probe.advance_bb);

    ctx.builder.position_at_end(probe.found_bb);
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_TOMBSTONE, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_sub(length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let removed = build_table_struct(ctx, function, entries_ptr, states_ptr, new_len, capacity)?;
    ret_struct(ctx, function, removed)?;

    ctx.builder.position_at_end(probe.not_found_bb);
    ret_struct(ctx, function, self_val)
}

pub(super) fn emit_map_get<'ctx>(
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

    let (entries_ptr, states_ptr, _length, capacity) =
        extract_table_fields(ctx, function, llvm_function)?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;
    let probe = emit_read_only_probe(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        capacity,
        key_val,
        hash_fn,
        eq_fn,
        key_basic_ty,
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

// ---------------------------------------------------------------------------
// Map.put / Set.insert — resize-or-not phi → probe → insert/update/already
// ---------------------------------------------------------------------------

/// Working state after the resize-or-not phi join: the live
/// `entries` / `states` / `capacity` for the probe loop (either the
/// originals when no resize fired, or the freshly-malloc'd
/// post-rehash buffers).
struct PostResize<'ctx> {
    capacity: IntValue<'ctx>,
    entries_ptr: PointerValue<'ctx>,
    states_ptr: PointerValue<'ctx>,
}

/// Emit the load-factor check, the resize-and-rehash path, and the
/// resize-or-not phi join. Returns the live `(entries, states, cap)`
/// for the probe block to consume. Builder ends positioned at the
/// post-join block.
#[allow(clippy::too_many_arguments)]
fn emit_resize_if_needed<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    entries_ptr: PointerValue<'ctx>,
    states_ptr: PointerValue<'ctx>,
    length: IntValue<'ctx>,
    capacity: IntValue<'ctx>,
    hash_fn: FunctionValue<'ctx>,
    key_basic_ty: BasicTypeEnum<'ctx>,
) -> Result<PostResize<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
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
        .build_int_add(length, i64_ty.const_int(1, false), "len_plus_1")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let lhs = ctx
        .builder
        .build_int_mul(len_plus_1, i64_ty.const_int(4, false), "lhs")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let rhs = ctx
        .builder
        .build_int_mul(capacity, i64_ty.const_int(3, false), "rhs")
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
        .build_int_mul(capacity, i64_ty.const_int(2, false), "new_cap")
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

    emit_rehash_loop(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        capacity,
        new_entries_ptr,
        new_states_ptr,
        new_cap,
        hash_fn,
        key_basic_ty,
    )?;

    // After rehash, free old buffers and branch to the join.
    let free = declare_free_extern(ctx);
    ctx.builder
        .build_call(free, &[entries_ptr.into()], "")
        .map_err(|e| codegen_err(format_args!("build_call free for `{}`", function.symbol), e))?;
    ctx.builder
        .build_call(free, &[states_ptr.into()], "")
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
        (&entries_ptr, no_resize_bb),
    ]);
    let sptr_phi = ctx
        .builder
        .build_phi(ptr_ty, "sptr_phi")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    sptr_phi.add_incoming(&[
        (&new_states_ptr, from_resize_bb),
        (&states_ptr, no_resize_bb),
    ]);
    let cap_phi = ctx
        .builder
        .build_phi(i64_ty, "cap_phi")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    cap_phi.add_incoming(&[(&new_cap, from_resize_bb), (&capacity, no_resize_bb)]);

    let _ = i8_ty;
    Ok(PostResize {
        capacity: cap_phi.as_basic_value().into_int_value(),
        entries_ptr: eptr_phi.as_basic_value().into_pointer_value(),
        states_ptr: sptr_phi.as_basic_value().into_pointer_value(),
    })
}

/// Rehash loop: for each `ri` in `0..old_capacity`, if the old
/// state is OCCUPIED, hash the old key, linear-probe in the new
/// buffer, memcpy the entry, mark new state OCCUPIED. Builder ends
/// positioned at the rehash-done block (caller's next emission
/// continues from there).
#[allow(clippy::too_many_arguments)]
fn emit_rehash_loop<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    old_entries: PointerValue<'ctx>,
    old_states: PointerValue<'ctx>,
    old_capacity: IntValue<'ctx>,
    new_entries: PointerValue<'ctx>,
    new_states: PointerValue<'ctx>,
    new_capacity: IntValue<'ctx>,
    hash_fn: FunctionValue<'ctx>,
    key_basic_ty: BasicTypeEnum<'ctx>,
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
        .build_int_compare(IntPredicate::UGE, ri, old_capacity, "ri_done")
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
            .build_gep(i8_ty, old_states, &[ri], "old_state_ptr")
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
    let old_entry_ptr = entry_pointer(ctx, function, old_entries, ri, layout.entry_size)?;
    let old_key = ctx
        .builder
        .build_load(key_basic_ty, old_entry_ptr, "old_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let old_hash = call_hash(ctx, function, hash_fn, old_key)?;
    let new_mask = ctx
        .builder
        .build_int_sub(new_capacity, i64_ty.const_int(1, false), "new_mask")
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
            .build_gep(i8_ty, new_states, &[rp_slot], "ns_ptr")
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
    let new_entry_ptr = entry_pointer(ctx, function, new_entries, rp_slot, layout.entry_size)?;
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

pub(super) fn emit_map_put<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let entries_ptr = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states_ptr = extract_pointer(ctx, function, self_val, 1, "states")?;
    let length = extract_int(ctx, function, self_val, 2, "len")?;
    let capacity = extract_int(ctx, function, self_val, 3, "cap")?;
    let key_val = nth_param(function, llvm_function, 1, "key")?;
    let value_val = nth_param(function, llvm_function, 2, "value")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;

    let post = emit_resize_if_needed(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        length,
        capacity,
        hash_fn,
        key_basic_ty,
    )?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        key_val,
        hash_fn,
        eq_fn,
        key_basic_ty,
    )?;

    // Update path: dup key found, overwrite value slot.
    ctx.builder.position_at_end(probe.update_bb);
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
    ctx.builder
        .build_store(val_ptr, value_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let updated = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        length,
        post.capacity,
    )?;
    ret_struct(ctx, function, updated)?;

    // Insert path: empty (or tombstone) slot, write key+value + state.
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, key_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let ins_val_ptr = unsafe {
        ctx.builder
            .build_gep(
                i8_ty,
                ins_ptr,
                &[i64_ty.const_int(layout.key_size, false)],
                "ins_val_ptr",
            )
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(ins_val_ptr, value_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, function, inserted)
}

pub(super) fn emit_set_insert<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let entries_ptr = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states_ptr = extract_pointer(ctx, function, self_val, 1, "states")?;
    let length = extract_int(ctx, function, self_val, 2, "len")?;
    let capacity = extract_int(ctx, function, self_val, 3, "cap")?;
    let item_val = nth_param(function, llvm_function, 1, "item")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;

    let post = emit_resize_if_needed(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        length,
        capacity,
        hash_fn,
        key_basic_ty,
    )?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        item_val,
        hash_fn,
        eq_fn,
        key_basic_ty,
    )?;

    // Duplicate-key path: Set returns self unchanged (no update).
    ctx.builder.position_at_end(probe.update_bb);
    let already = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        length,
        post.capacity,
    )?;
    ret_struct(ctx, function, already)?;

    // Insert path: empty (or tombstone) slot, write entry + state.
    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, item_val)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    ret_struct(ctx, function, inserted)
}

/// Output of [`emit_insert_probe`]: which `update` vs `insert` block
/// each outcome reached, plus the `pidx` / `e_ptr` / `s_ptr` SSA
/// values the per-collection tail consumes.
struct InsertProbe<'ctx> {
    e_ptr: PointerValue<'ctx>,
    insert_bb: BasicBlock<'ctx>,
    pidx: IntValue<'ctx>,
    s_ptr: PointerValue<'ctx>,
    update_bb: BasicBlock<'ctx>,
}

/// Emit a probe loop that returns to the caller at either
/// `update_bb` (existing key hit — caller decides what to do) or
/// `insert_bb` (empty/tombstone slot — caller writes the new
/// entry). On entry the builder must sit at a single predecessor;
/// on return it sits at an unspecified position and the caller
/// branches to `update_bb` / `insert_bb` via `position_at_end`.
#[allow(clippy::too_many_arguments)]
fn emit_insert_probe<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    post: &PostResize<'ctx>,
    key_val: BasicValueEnum<'ctx>,
    hash_fn: FunctionValue<'ctx>,
    eq_fn: FunctionValue<'ctx>,
    key_basic_ty: BasicTypeEnum<'ctx>,
) -> Result<InsertProbe<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "emit_insert_probe called with no insertion block for `{}`",
            function.symbol,
        ))
    })?;
    let hash_val = call_hash(ctx, function, hash_fn, key_val)?;
    let mask = ctx
        .builder
        .build_int_sub(post.capacity, i64_ty.const_int(1, false), "mask")
        .map_err(|e| codegen_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let start_slot = ctx
        .builder
        .build_and(hash_val, mask, "start_slot")
        .map_err(|e| codegen_err(format_args!("build_and for `{}`", function.symbol), e))?;

    let probe_loop_bb = ctx.context.append_basic_block(llvm_function, "probe_loop");
    let check_occ_bb = ctx.context.append_basic_block(llvm_function, "check_occ");
    let compare_key_bb = ctx.context.append_basic_block(llvm_function, "compare_key");
    let update_bb = ctx.context.append_basic_block(llvm_function, "update");
    let insert_bb = ctx.context.append_basic_block(llvm_function, "insert");
    let advance_bb = ctx.context.append_basic_block(llvm_function, "advance");

    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;
    ctx.builder.position_at_end(probe_loop_bb);
    let pidx_phi = ctx
        .builder
        .build_phi(i64_ty, "pidx")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    pidx_phi.add_incoming(&[(&start_slot, entry_block)]);
    let pidx = pidx_phi.as_basic_value().into_int_value();

    let s_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, post.states_ptr, &[pidx], "s_ptr")
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
        .build_conditional_branch(is_empty, insert_bb, check_occ_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(check_occ_bb);
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
        .build_conditional_branch(is_occ, compare_key_bb, insert_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(compare_key_bb);
    let e_ptr = entry_pointer(ctx, function, post.entries_ptr, pidx, layout.entry_size)?;
    let existing_key = ctx
        .builder
        .build_load(key_basic_ty, e_ptr, "existing_key")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let keys_equal = call_eq(ctx, function, eq_fn, key_val, existing_key)?;
    ctx.builder
        .build_conditional_branch(keys_equal, update_bb, advance_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(advance_bb);
    let wrapped = advance_slot(ctx, function, pidx, mask)?;
    pidx_phi.add_incoming(&[(&wrapped, advance_bb)]);
    ctx.builder
        .build_unconditional_branch(probe_loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    Ok(InsertProbe {
        e_ptr,
        insert_bb,
        pidx,
        s_ptr,
        update_bb,
    })
}

// ---------------------------------------------------------------------------
// Set.from_list — walk the list's buffer and call Set.insert per element
// ---------------------------------------------------------------------------

pub(super) fn emit_set_from_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().unwrap();
    let elem_basic_ty = ir_basic_type(ctx, layout.key_ty)?;

    let list_val = match nth_param(function, llvm_function, 0, "list")? {
        BasicValueEnum::StructValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Set.from_list expected list struct on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let list_ty = list_value_type(ctx);
    let _ = list_ty;
    let list_ptr = ctx
        .builder
        .build_extract_value(list_val, 0, "list_ptr")
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_pointer_value();
    let list_len = ctx
        .builder
        .build_extract_value(list_val, 1, "list_len")
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_int_value();

    // Mint an empty set, then loop and insert each element. We
    // call back into `emit_new` + a freshly-built insert helper
    // rather than the user-facing `Set.new` symbol because the
    // declared-function table may not have either yet at the time
    // `from_list` defines its body.
    let init_set = build_empty_table(ctx, function, layout.entry_size)?;
    let set_alloca = ctx
        .builder
        .build_alloca(hashtable_value_type(ctx), "set_acc")
        .map_err(|e| codegen_err(format_args!("build_alloca for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(set_alloca, init_set)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;

    let loop_bb = ctx.context.append_basic_block(llvm_function, "loop");
    let body_bb = ctx.context.append_basic_block(llvm_function, "body");
    let done_bb = ctx.context.append_basic_block(llvm_function, "done");
    ctx.builder
        .build_unconditional_branch(loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(loop_bb);
    let i_phi = ctx
        .builder
        .build_phi(i64_ty, "i")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    i_phi.add_incoming(&[(&i64_ty.const_zero(), entry_block)]);
    let i_val = i_phi.as_basic_value().into_int_value();
    let done = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, i_val, list_len, "done")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(done, done_bb, body_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(body_bb);
    let byte_offset = ctx
        .builder
        .build_int_mul(
            i_val,
            i64_ty.const_int(layout.entry_size, false),
            "byte_off",
        )
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, list_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let elem_val = ctx
        .builder
        .build_load(elem_basic_ty, elem_ptr, "elem_val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let current = ctx
        .builder
        .build_load(hashtable_value_type(ctx), set_alloca, "cur_set")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let updated = call_set_insert_inline(ctx, function, llvm_function, layout, current, elem_val)?;
    ctx.builder
        .build_store(set_alloca, updated)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let next_i = ctx
        .builder
        .build_int_add(i_val, i64_ty.const_int(1, false), "next_i")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let body_tail = ctx.builder.get_insert_block().unwrap();
    i_phi.add_incoming(&[(&next_i, body_tail)]);
    ctx.builder
        .build_unconditional_branch(loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(done_bb);
    let final_set = ctx
        .builder
        .build_load(hashtable_value_type(ctx), set_alloca, "final_set")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_struct_value();
    ret_struct(ctx, function, final_set)
}

/// Inline the `Set.insert` body at a call site. v1 emitted a
/// sibling function and called it; this avoids that round-trip by
/// inlining — the per-method declared-function index isn't
/// populated for the freshly-monomorphized `Set.insert` at the
/// point where `from_list`'s body is being emitted.
fn call_set_insert_inline<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    current: BasicValueEnum<'ctx>,
    item: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let current_struct = current.into_struct_value();
    let entries_ptr = extract_pointer(ctx, function, current_struct, 0, "entries")?;
    let states_ptr = extract_pointer(ctx, function, current_struct, 1, "states")?;
    let length = extract_int(ctx, function, current_struct, 2, "len")?;
    let capacity = extract_int(ctx, function, current_struct, 3, "cap")?;
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, layout.key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, layout.key_ty)?;

    let post = emit_resize_if_needed(
        ctx,
        function,
        llvm_function,
        layout,
        entries_ptr,
        states_ptr,
        length,
        capacity,
        hash_fn,
        key_basic_ty,
    )?;
    let probe = emit_insert_probe(
        ctx,
        function,
        llvm_function,
        layout,
        &post,
        item,
        hash_fn,
        eq_fn,
        key_basic_ty,
    )?;
    // After `emit_insert_probe` returns, the builder is parked on
    // the (already-terminated) `advance` block — appending any
    // instruction here would land it after a terminator, which is
    // malformed IR. Build the merge block fresh and stitch the
    // two outcome paths together with a PHI instead of an alloca.
    let merge_bb = ctx
        .context
        .append_basic_block(llvm_function, "insert_merge");

    ctx.builder.position_at_end(probe.update_bb);
    let dup_result = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        length,
        post.capacity,
    )?;
    let update_tail = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "call_set_insert_inline lost update insertion block on `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, item)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted_result = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    let insert_tail = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "call_set_insert_inline lost insert insertion block on `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(merge_bb);
    let phi = ctx
        .builder
        .build_phi(hashtable_value_type(ctx), "set_insert_val")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    phi.add_incoming(&[(&dup_result, update_tail), (&inserted_result, insert_tail)]);
    Ok(phi.as_basic_value())
}

// ---------------------------------------------------------------------------
// Shared low-level helpers
// ---------------------------------------------------------------------------

/// Byte size of an [`IRType`] on the host triple, routed through
/// the same target-data the rest of the layout pipeline reads
/// (so hash-table entry sizes match the LLVM-emitted field sizes
/// byte-for-byte).
pub(super) fn ir_byte_size<'ctx>(ctx: &EmitContext<'ctx>, ty: &IRType) -> Result<u64, LlvmError> {
    let basic = ir_basic_type(ctx, ty)?;
    Ok(ctx.layouts.target_data.get_abi_size(&basic))
}

fn build_empty_table<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    entry_size: u64,
) -> Result<StructValue<'ctx>, LlvmError> {
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let capacity = i64_ty.const_int(INITIAL_CAPACITY, false);
    let entries_bytes = ctx
        .builder
        .build_int_mul(
            capacity,
            i64_ty.const_int(entry_size, false),
            "entries_bytes",
        )
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
    build_table_struct(
        ctx,
        function,
        entries_ptr,
        states_ptr,
        i64_ty.const_zero(),
        capacity,
    )
}

fn build_table_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    entries_ptr: PointerValue<'ctx>,
    states_ptr: PointerValue<'ctx>,
    length: IntValue<'ctx>,
    capacity: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let table_ty = hashtable_value_type(ctx);
    let s = ctx
        .builder
        .build_insert_value(table_ty.get_undef(), entries_ptr, 0, "with_entries")
        .map_err(|e| {
            codegen_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, states_ptr, 1, "with_states")
        .map_err(|e| {
            codegen_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, length, 2, "with_len")
        .map_err(|e| {
            codegen_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, capacity, 3, "with_cap")
        .map_err(|e| {
            codegen_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    Ok(s)
}

fn call_malloc<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    malloc: FunctionValue<'ctx>,
    bytes: IntValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_call(malloc, &[bytes.into()], name)
        .map_err(|e| {
            codegen_err(
                format_args!("build_call malloc for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "malloc returned no value for `{}`",
                function.symbol
            ))
        })
        .map(|v| v.into_pointer_value())
}

fn call_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    hash_fn: FunctionValue<'ctx>,
    key: BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_call(hash_fn, &[key.into()], "key_hash")
        .map_err(|e| codegen_err(format_args!("build_call hash for `{}`", function.symbol), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!("hash returned no value for `{}`", function.symbol))
        })
        .map(|v| v.into_int_value())
}

fn call_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    eq_fn: FunctionValue<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_call(eq_fn, &[lhs.into(), rhs.into()], "keys_eq")
        .map_err(|e| codegen_err(format_args!("build_call eq for `{}`", function.symbol), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!("eq returned no value for `{}`", function.symbol))
        })
        .map(|v| v.into_int_value())
}

fn advance_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    slot: IntValue<'ctx>,
    mask: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let next = ctx
        .builder
        .build_int_add(slot, i64_ty.const_int(1, false), "next_slot")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    ctx.builder
        .build_and(next, mask, "wrapped_slot")
        .map_err(|e| codegen_err(format_args!("build_and for `{}`", function.symbol), e))
}

fn entry_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    entries_ptr: PointerValue<'ctx>,
    slot: IntValue<'ctx>,
    entry_size: u64,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let byte_offset = ctx
        .builder
        .build_int_mul(slot, i64_ty.const_int(entry_size, false), "byte_off")
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    unsafe {
        ctx.builder
            .build_gep(i8_ty, entries_ptr, &[byte_offset], "entry_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))
    }
}

fn extract_table_fields<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<
    (
        PointerValue<'ctx>,
        PointerValue<'ctx>,
        IntValue<'ctx>,
        IntValue<'ctx>,
    ),
    LlvmError,
> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    let entries = extract_pointer(ctx, function, self_val, 0, "entries")?;
    let states = extract_pointer(ctx, function, self_val, 1, "states")?;
    let len = extract_int(ctx, function, self_val, 2, "len")?;
    let cap = extract_int(ctx, function, self_val, 3, "cap")?;
    Ok((entries, states, len, cap))
}

fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    table: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(ctx
        .builder
        .build_extract_value(table, index, name)
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_int_value())
}

fn extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    table: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    Ok(ctx
        .builder
        .build_extract_value(table, index, name)
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_pointer_value())
}

fn nth_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing param `{name}` (#{index}) on `{}`",
            function.symbol,
        ))
    })
}

fn nth_hashtable<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<StructValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::StructValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected hashtable struct for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn ret_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: StructValue<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn ret_basic<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn codegen_err<E: std::fmt::Display>(args: std::fmt::Arguments<'_>, e: E) -> LlvmError {
    inkwell_err(args, e)
}

/// Resolve the Hash + Equality intrinsics for `key_ty` via the
/// declared-function index. The lift pass stamps every primitive
/// receiver's `hash` / `eq` as a `Global.<Type>.hash`-style
/// `IRSymbol`, and per-struct impls follow the same shape with
/// the struct's already-mangled symbol as the receiver root, so
/// the lookup is a single index hit per side. Misses surface as
/// a clean codegen error rather than panicking — the surface
/// language can declare a `Map<K, _>` over a `K` that doesn't
/// implement `Hash`, so this branch must produce an actionable
/// diagnostic.
fn resolve_hash_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    key_ty: &IRType,
) -> Result<(FunctionValue<'ctx>, FunctionValue<'ctx>), LlvmError> {
    let receiver = hash_receiver_symbol(key_ty).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "type `{key_ty:?}` does not implement Hash (no receiver symbol) for `{}`",
            function.symbol,
        ))
    })?;
    let hash_symbol = mangled_method_name(&receiver, &[], "hash", &[]);
    let eq_symbol = mangled_method_name(&receiver, &[], "eq", &[]);
    let hash_fn = ctx.declared_function(&hash_symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "type `{key_ty:?}` does not implement Hash (no `{}` function) for `{}`",
            hash_symbol, function.symbol,
        ))
    })?;
    let eq_fn = ctx.declared_function(&eq_symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "type `{key_ty:?}` does not implement Equality (no `{}` function) for `{}`",
            eq_symbol, function.symbol,
        ))
    })?;
    Ok((hash_fn, eq_fn))
}

/// Mint the receiver `IRSymbol` for `<key_ty>.hash` / `<key_ty>.eq`
/// lookups. Primitives root at `Global.<Type>` via the lift pass'
/// convention; struct types reuse their already-mangled symbol so
/// per-monomorphization impls (`MyApp.Pair_$Int.String$.hash`) hit
/// the same lookup path.
fn hash_receiver_symbol(key_ty: &IRType) -> Option<IRSymbol> {
    Some(match key_ty {
        IRType::Bool => global_primitive_symbol("Bool"),
        IRType::Int8 => global_primitive_symbol("Int8"),
        IRType::Int16 => global_primitive_symbol("Int16"),
        IRType::Int32 => global_primitive_symbol("Int32"),
        IRType::Int64 => global_primitive_symbol("Int"),
        IRType::UInt8 => global_primitive_symbol("UInt8"),
        IRType::UInt16 => global_primitive_symbol("UInt16"),
        IRType::UInt32 => global_primitive_symbol("UInt32"),
        IRType::UInt64 => global_primitive_symbol("UInt64"),
        IRType::String => global_primitive_symbol("String"),
        IRType::Struct(symbol) => symbol.clone(),
        _ => return None,
    })
}

/// Recover the enum `IRSymbol` from a slot that the lowering pass
/// guarantees is an `IRType::Enum`. Defensive (codegen-error, not
/// panic) so an upstream slip surfaces as a diagnostic.
fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
    label: &str,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "{label} expected an enum-typed slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}
