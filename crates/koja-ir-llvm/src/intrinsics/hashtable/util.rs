//! Low-level building blocks shared across every hashtable submodule.
//!
//! Two flavours of helper live here:
//!
//! - Instruction wrappers ([`call_malloc`], [`call_hash`], [`call_eq`],
//!   [`call_clone`], [`advance_slot`], [`entry_pointer`],
//!   [`build_table_struct`], [`build_empty_table`]) that bundle the
//!   inkwell builder calls + `codegen_err` plumbing into a single
//!   line at the call site.
//! - Symbol / type resolution ([`resolve_hash_eq`],
//!   [`resolve_clone_fn`], [`expect_enum_symbol`]) for crossing from
//!   sealed IR ([`IRType`] / [`IRSymbol`]) to monomorphized inkwell
//!   [`FunctionValue`]s via [`koja_ir::mangling`].
//!
//! Everything is `pub(super)`: visible to sibling submodules, hidden
//! from the rest of the crate.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::mangling::{global_primitive_symbol, mangled_method_name};
use koja_ir::{IRFunction, IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{declare_malloc_extern, declare_memset_extern};
use crate::types::{hashtable_value_type, ir_basic_type};

use super::INITIAL_CAPACITY;

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

pub(super) fn build_table_struct<'ctx>(
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

pub(super) fn call_malloc<'ctx>(
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

pub(super) fn call_hash<'ctx>(
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

pub(super) fn call_eq<'ctx>(
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

pub(super) fn call_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    clone_fn: FunctionValue<'ctx>,
    value: BasicValueEnum<'ctx>,
    name: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    ctx.builder
        .build_call(clone_fn, &[value.into()], name)
        .map_err(|e| {
            codegen_err(
                format_args!("build_call clone for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!("clone returned no value for `{}`", function.symbol))
        })
}

pub(super) fn advance_slot<'ctx>(
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

pub(super) fn entry_pointer<'ctx>(
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

pub(super) fn extract_table_fields<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<TableSnapshot<'ctx>, LlvmError> {
    let self_val = nth_hashtable(function, llvm_function, 0, "self")?;
    Ok(TableSnapshot {
        entries_ptr: extract_pointer(ctx, function, self_val, 0, "entries")?,
        states_ptr: extract_pointer(ctx, function, self_val, 1, "states")?,
        length: extract_int(ctx, function, self_val, 2, "len")?,
        capacity: extract_int(ctx, function, self_val, 3, "cap")?,
    })
}

pub(super) fn extract_int<'ctx>(
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

pub(super) fn extract_pointer<'ctx>(
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

pub(super) fn ret_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: StructValue<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))
}

pub(super) fn ret_basic<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| codegen_err(format_args!("build_return for `{}`", function.symbol), e))
}

pub(super) fn codegen_err<E: std::fmt::Display>(args: std::fmt::Arguments<'_>, e: E) -> LlvmError {
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

/// Resolve the monomorphized `clone` function for `elem_ty` — the
/// universal-`Clone` contract guarantees every type lands here with
/// a callable impl (intrinsic for `String` / `Binary` / `Bits` /
/// `Map` / `Set`, hand-written in `.koja` for primitives + `List` +
/// `CPtr`, synthesized for user structs and enums). Surfaces a clean
/// codegen error when the receiver shape isn't one we know how to
/// mangle (e.g. function-typed `V` in a `Map<K, fn () -> R>` —
/// that's a follow-up).
pub(super) fn resolve_clone_fn<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    elem_ty: &IRType,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    let (template, args) = clone_receiver_symbol(elem_ty).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "type `{elem_ty:?}` does not have a callable `clone` shape for `{}`",
            function.symbol,
        ))
    })?;
    let clone_symbol = mangled_method_name(&template, &args, "clone", &[]);
    ctx.declared_function(&clone_symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "type `{elem_ty:?}` does not implement Clone (no `{}` function) for `{}`",
            clone_symbol, function.symbol,
        ))
    })
}

/// Receiver template + args for [`mangled_method_name`] on `clone`.
/// Generic shapes (`List<T>`, `Map<K, V>`, `Set<T>`, `CPtr<T>`)
/// reconstruct the receiver pair so the result mangles through the
/// same path as the original `impl Clone for T` block. Already-
/// mangled struct / enum symbols pass through verbatim with empty
/// args — monomorphization stamps the full generic instantiation
/// into the symbol root before codegen sees the type.
fn clone_receiver_symbol(elem_ty: &IRType) -> Option<(IRSymbol, Vec<IRType>)> {
    Some(match elem_ty {
        IRType::Binary => (global_primitive_symbol("Binary"), Vec::new()),
        IRType::Bits => (global_primitive_symbol("Bits"), Vec::new()),
        IRType::Bool => (global_primitive_symbol("Bool"), Vec::new()),
        IRType::CPtr(inner) => (global_primitive_symbol("CPtr"), vec![(**inner).clone()]),
        IRType::Enum(symbol) | IRType::Struct(symbol) => (symbol.clone(), Vec::new()),
        IRType::Float32 => (global_primitive_symbol("Float32"), Vec::new()),
        IRType::Float64 => (global_primitive_symbol("Float"), Vec::new()),
        IRType::Int8 => (global_primitive_symbol("Int8"), Vec::new()),
        IRType::Int16 => (global_primitive_symbol("Int16"), Vec::new()),
        IRType::Int32 => (global_primitive_symbol("Int32"), Vec::new()),
        IRType::Int64 => (global_primitive_symbol("Int"), Vec::new()),
        IRType::List(inner) => (global_primitive_symbol("List"), vec![(**inner).clone()]),
        IRType::Map { key, value } => (
            global_primitive_symbol("Map"),
            vec![(**key).clone(), (**value).clone()],
        ),
        IRType::Set(inner) => (global_primitive_symbol("Set"), vec![(**inner).clone()]),
        IRType::String => (global_primitive_symbol("String"), Vec::new()),
        IRType::UInt8 => (global_primitive_symbol("UInt8"), Vec::new()),
        IRType::UInt16 => (global_primitive_symbol("UInt16"), Vec::new()),
        IRType::UInt32 => (global_primitive_symbol("UInt32"), Vec::new()),
        IRType::UInt64 => (global_primitive_symbol("UInt64"), Vec::new()),
        IRType::Unit => (global_primitive_symbol("Unit"), Vec::new()),
        IRType::Function { .. } | IRType::Indirect(_) | IRType::Union { .. } => return None,
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
