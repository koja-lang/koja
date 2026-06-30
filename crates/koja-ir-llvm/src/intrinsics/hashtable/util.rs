//! Low-level building blocks shared across every hashtable submodule.
//!
//! Two flavours of helper live here:
//!
//! - Instruction wrappers ([`call_malloc`], [`call_hash`], [`call_eq`],
//!   [`advance_slot`], [`entry_pointer`], [`build_table_struct`],
//!   [`build_empty_table`]) that bundle the inkwell builder calls +
//!   error plumbing into a single line at the call site.
//! - Symbol / type resolution ([`resolve_hash_eq`],
//!   [`expect_enum_symbol`]) for crossing from sealed IR
//!   ([`IRType`] / [`IRSymbol`]) to monomorphized inkwell
//!   [`FunctionValue`]s via [`koja_ir::mangling`].
//!
//! Everything is `pub(super)`: visible to sibling submodules, hidden
//! from the rest of the crate.

use inkwell::IntPredicate;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::mangling::{global_primitive_symbol, mangled_method_name};
use koja_ir::{IRFunction, IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::acquire_in_slot;
use crate::runtime::{declare_malloc_extern, declare_memset_extern};
use crate::types::{hashtable_value_type, ir_basic_type};

use super::{HashtableLayout, INITIAL_CAPACITY, STATE_OCCUPIED};

/// Live state of one hashtable: the two buffer pointers plus the
/// occupancy + capacity ints. Freshly extracted from `self` by
/// [`extract_table_fields`], returned (possibly with swapped
/// buffers and grown capacity) from a resize, or threaded by hand
/// through multi-step write paths. `length` is invariant across
/// the resize-or-not phi join — the bump-on-insert happens after
/// probing, never inside the resize.
pub(super) struct TableSnapshot<'ctx> {
    pub entries_ptr: PointerValue<'ctx>,
    pub states_ptr: PointerValue<'ctx>,
    pub length: IntValue<'ctx>,
    pub capacity: IntValue<'ctx>,
}

/// K-side intrinsics resolved once per `Map` / `Set` method
/// emission: the monomorphized `hash` / `eq` functions plus the
/// LLVM basic type for `K`. Probe paths read all three; rehash
/// only needs `hash_fn` + `key_basic_ty` because moving an
/// already-bucketed key into a larger buffer doesn't compare
/// against existing slots.
pub(super) struct KeyHashOps<'ctx> {
    pub hash_fn: FunctionValue<'ctx>,
    pub eq_fn: FunctionValue<'ctx>,
    pub key_basic_ty: BasicTypeEnum<'ctx>,
}

/// Byte size of an [`IRType`] on the host triple, routed through
/// the same target-data the rest of the layout pipeline reads
/// (so hash-table entry sizes match the LLVM-emitted field sizes
/// byte-for-byte).
pub(crate) fn ir_byte_size<'ctx>(ctx: &EmitContext<'ctx>, ty: &IRType) -> Result<u64, LlvmError> {
    let basic = ir_basic_type(ctx, ty)?;
    Ok(ctx.layouts.target_data.get_abi_size(&basic))
}

/// Bundle [`resolve_hash_eq`] and [`ir_basic_type`] into a single
/// resolution step so every emit method that probes a `K`-keyed
/// table can derive its [`KeyHashOps`] in one line.
pub(super) fn resolve_key_hash_ops<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    key_ty: &IRType,
) -> Result<KeyHashOps<'ctx>, LlvmError> {
    let (hash_fn, eq_fn) = resolve_hash_eq(ctx, function, key_ty)?;
    let key_basic_ty = ir_basic_type(ctx, key_ty)?;
    Ok(KeyHashOps {
        hash_fn,
        eq_fn,
        key_basic_ty,
    })
}

pub(super) fn build_empty_table<'ctx>(
    ctx: &EmitContext<'ctx>,
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
    build_table_struct(ctx, entries_ptr, states_ptr, i64_ty.const_zero(), capacity)
}

pub(super) fn build_table_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    entries_ptr: PointerValue<'ctx>,
    states_ptr: PointerValue<'ctx>,
    length: IntValue<'ctx>,
    capacity: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let table_ty = hashtable_value_type(ctx);
    let s = ctx
        .builder
        .build_insert_value(table_ty.get_undef(), entries_ptr, 0, "with_entries")
        .or_ice()?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, states_ptr, 1, "with_states")
        .or_ice()?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, length, 2, "with_len")
        .or_ice()?
        .into_struct_value();
    let s = ctx
        .builder
        .build_insert_value(s, capacity, 3, "with_cap")
        .or_ice()?
        .into_struct_value();
    Ok(s)
}

#[track_caller]
pub(super) fn call_malloc<'ctx>(
    ctx: &EmitContext<'ctx>,
    malloc: FunctionValue<'ctx>,
    bytes: IntValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    Ok(ctx
        .call_basic(malloc, &[bytes.into()], name)?
        .into_pointer_value())
}

/// Copy-on-write clone of a table's buffers into fresh allocations.
/// Under value semantics every hashtable mutator clones the entries +
/// states buffers before writing, so a binding shared by assignment is
/// never mutated in place through another alias. After the `memcpy`s
/// every occupied bucket is *acquired* — `rc++` a heap-leaf key/value,
/// deep-clone a composite — so the copy owns independent references and
/// the shared payloads don't double-free once both tables are reclaimed
/// by drop glue.
pub(super) fn clone_table_buffers<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    src: &TableSnapshot<'ctx>,
) -> Result<TableSnapshot<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let entries_bytes = ctx
        .builder
        .build_int_mul(
            src.capacity,
            i64_ty.const_int(layout.entry_size, false),
            "entries_bytes",
        )
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let dst_entries = call_malloc(ctx, malloc, entries_bytes, "cow_entries")?;
    let dst_states = call_malloc(ctx, malloc, src.capacity, "cow_states")?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[
                dst_entries.into(),
                src.entries_ptr.into(),
                entries_bytes.into(),
            ],
            "",
        )
        .or_ice()?;
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
        .or_ice()?;
    let dst = TableSnapshot {
        entries_ptr: dst_entries,
        states_ptr: dst_states,
        length: src.length,
        capacity: src.capacity,
    };
    acquire_occupied_entries(ctx, llvm_function, layout, &dst)?;
    Ok(dst)
}

/// Acquire the key (and, for `Map`, the value) of every occupied bucket
/// in `table` — the per-element half of a copy-on-write clone. A no-op
/// walk when both key and value own no heap.
fn acquire_occupied_entries<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    table: &TableSnapshot<'ctx>,
) -> Result<(), LlvmError> {
    occupied_loop(
        ctx,
        llvm_function,
        table.states_ptr,
        table.capacity,
        "cow",
        |ctx, slot| {
            let entry_ptr = entry_pointer(ctx, table.entries_ptr, slot, layout.entry_size)?;
            acquire_in_slot(ctx, layout.key_ty, entry_ptr)?;
            if let Some(value_ty) = layout.value_ty {
                let value_ptr = value_slot(ctx, entry_ptr, layout.key_size)?;
                acquire_in_slot(ctx, value_ty, value_ptr)?;
            }
            Ok(())
        },
    )
}

/// Pointer to the value half of a `Map` bucket — `key_size` bytes past
/// the entry base (the key sits at offset 0).
#[track_caller]
pub(super) fn value_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    entry_ptr: PointerValue<'ctx>,
    key_size: u64,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let offset = ctx.context.i64_type().const_int(key_size, false);
    unsafe {
        ctx.builder
            .build_gep(i8_ty, entry_ptr, &[offset], "val_ptr")
            .or_ice()
    }
}

/// Emit `for slot in 0..capacity { if states[slot] == OCCUPIED { body } }`
/// over a table's buckets. The straight-line `body` runs once into the
/// per-occupied-bucket block; this helper owns the counter, the range
/// guard, the occupancy test, and the back-edge. Shared by the
/// copy-on-write element acquire here and the `Map` / `Set` clone /
/// drop glue ([`crate::emit::collection_glue`]).
pub(crate) fn occupied_loop<'ctx>(
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
        .or_ice()?;
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

    ctx.builder.build_unconditional_branch(head).or_ice()?;
    ctx.builder.position_at_end(head);
    let index = ctx
        .builder
        .build_load(i64_ty, counter, &format!("{label}.idx"))
        .or_ice()?
        .into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, capacity, &format!("{label}.cmp"))
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(in_range, check, exit)
        .or_ice()?;

    ctx.builder.position_at_end(check);
    let state_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, states, &[index], &format!("{label}.state_ptr"))
            .or_ice()?
    };
    let state = ctx
        .builder
        .build_load(i8_ty, state_ptr, &format!("{label}.state"))
        .or_ice()?
        .into_int_value();
    let is_occupied = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            state,
            i8_ty.const_int(STATE_OCCUPIED, false),
            &format!("{label}.is_occupied"),
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(is_occupied, occupied, next)
        .or_ice()?;

    ctx.builder.position_at_end(occupied);
    body(ctx, index)?;
    ctx.builder.build_unconditional_branch(next).or_ice()?;

    ctx.builder.position_at_end(next);
    let incremented = ctx
        .builder
        .build_int_add(index, i64_ty.const_int(1, false), &format!("{label}.inc"))
        .or_ice()?;
    ctx.builder.build_store(counter, incremented).or_ice()?;
    ctx.builder.build_unconditional_branch(head).or_ice()?;

    ctx.builder.position_at_end(exit);
    Ok(())
}

#[track_caller]
pub(super) fn call_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    hash_fn: FunctionValue<'ctx>,
    key: BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(ctx
        .call_basic(hash_fn, &[key.into()], "key_hash")?
        .into_int_value())
}

#[track_caller]
pub(super) fn call_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    eq_fn: FunctionValue<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(ctx
        .call_basic(eq_fn, &[lhs.into(), rhs.into()], "keys_eq")?
        .into_int_value())
}

pub(super) fn advance_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    slot: IntValue<'ctx>,
    mask: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let next = ctx
        .builder
        .build_int_add(slot, i64_ty.const_int(1, false), "next_slot")
        .or_ice()?;
    ctx.builder.build_and(next, mask, "wrapped_slot").or_ice()
}

pub(super) fn entry_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    entries_ptr: PointerValue<'ctx>,
    slot: IntValue<'ctx>,
    entry_size: u64,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let byte_offset = ctx
        .builder
        .build_int_mul(slot, i64_ty.const_int(entry_size, false), "byte_off")
        .or_ice()?;
    unsafe {
        ctx.builder
            .build_gep(i8_ty, entries_ptr, &[byte_offset], "entry_ptr")
            .or_ice()
    }
}

pub(super) fn extract_table_fields<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<TableSnapshot<'ctx>, LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    Ok(TableSnapshot {
        entries_ptr: extract_pointer(ctx, self_val, 0, "entries")?,
        states_ptr: extract_pointer(ctx, self_val, 1, "states")?,
        length: extract_int(ctx, self_val, 2, "len")?,
        capacity: extract_int(ctx, self_val, 3, "cap")?,
    })
}

#[track_caller]
pub(super) fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    table: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(ctx
        .builder
        .build_extract_value(table, index, name)
        .or_ice()?
        .into_int_value())
}

#[track_caller]
pub(super) fn extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    table: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    Ok(ctx
        .builder
        .build_extract_value(table, index, name)
        .or_ice()?
        .into_pointer_value())
}

pub(super) fn nth_param<'ctx>(
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

pub(super) fn nth_hashtable<'ctx>(
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

#[track_caller]
pub(super) fn ret_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: StructValue<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

#[track_caller]
pub(super) fn ret_basic<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
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
pub(super) fn resolve_hash_eq<'ctx>(
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
        IRType::Bool => global_primitive_symbol(&["Bool"]),
        IRType::Int8 => global_primitive_symbol(&["Int8"]),
        IRType::Int16 => global_primitive_symbol(&["Int16"]),
        IRType::Int32 => global_primitive_symbol(&["Int32"]),
        IRType::Int64 => global_primitive_symbol(&["Int"]),
        IRType::UInt8 => global_primitive_symbol(&["UInt8"]),
        IRType::UInt16 => global_primitive_symbol(&["UInt16"]),
        IRType::UInt32 => global_primitive_symbol(&["UInt32"]),
        IRType::UInt64 => global_primitive_symbol(&["UInt64"]),
        IRType::String => global_primitive_symbol(&["String"]),
        IRType::Struct(symbol) => symbol.clone(),
        _ => return None,
    })
}

/// Recover the enum `IRSymbol` from a slot that the lowering pass
/// guarantees is an `IRType::Enum`. Defensive (codegen-error, not
/// panic) so an upstream slip surfaces as a diagnostic.
pub(super) fn expect_enum_symbol<'ty>(
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
