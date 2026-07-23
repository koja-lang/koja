//! `List<T>` family: heap-backed dynamic array. Layout is
//! `{ buf_ptr: i8*, len: i64, cap: i64 }`, passed by value. Element
//! storage lives off-heap behind `buf_ptr`. Methods malloc / realloc
//! / memcpy via libc directly, no Rust-side runtime helpers.
//!
//! Element-size-parameterized: each method computes `elem_size` from
//! the `IRType::List(_)` inner type carried on the function's
//! signature, then generates the same shape of IR regardless of `T`.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, ListMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::element::{acquire_buffer, acquire_value, element_slot, release_in_slot};
use crate::runtime::declare_malloc_extern;
use crate::types::{ir_basic_type, list_value_type};

/// `Option<T>` variant tags as the stdlib decls them: `Some` first
/// (tag 0), then `None`. Both intrinsic emitters that need to mint
/// option values (`List.get`, `List.pop`) go through these so the
/// numeric tags aren't sprinkled across call sites.
const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

/// Initial buffer capacity for `List.new`. Matches v1.
const INITIAL_CAPACITY: u64 = 8;

pub(super) fn emit_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ListMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match method {
        ListMethod::Append => emit_append(ctx, function, llvm_function),
        ListMethod::Concat => emit_concat(ctx, function, llvm_function),
        ListMethod::EmptyQ => emit_empty_q(ctx, function, llvm_function),
        ListMethod::FromList => emit_from_list(ctx, function, llvm_function),
        ListMethod::Get => emit_get(ctx, function, llvm_function, entry),
        ListMethod::Length => emit_length(ctx, function, llvm_function),
        ListMethod::New => emit_new(ctx, function),
        ListMethod::Pop => emit_pop(ctx, function, llvm_function, entry),
        ListMethod::ReplaceAt => emit_replace_at(ctx, function, llvm_function, entry),
        ListMethod::Slice => emit_slice(ctx, function, llvm_function, entry),
    }
}

/// Resolve the element `T` for a `List<T>` intrinsic. `new` carries
/// it on the return type. Every other method has `self: List<T>` as
/// `params[0]` (or `params[1]` for `concat`'s `other`, but both
/// share the same `T`).
fn element(method: ListMethod, function: &IRFunction) -> Result<&IRType, LlvmError> {
    let candidate = match method {
        ListMethod::New => &function.return_type,
        _ => &function.params[0].ty,
    };
    match candidate {
        IRType::List(inner) => Ok(inner),
        other => Err(LlvmError::Codegen(format!(
            "List.{method:?} expected a `List<T>` slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

fn element_byte_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    method: ListMethod,
) -> Result<IntValue<'ctx>, LlvmError> {
    let elem_ty = element(method, function)?;
    let basic = ir_basic_type(ctx, elem_ty)?;
    basic.size_of().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "List.{method:?} cannot compute size of element `{elem_ty:?}` (symbol `{}`)",
            function.symbol,
        ))
    })
}

fn emit_new<'ctx>(ctx: &EmitContext<'ctx>, function: &IRFunction) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let initial_cap = i64_ty.const_int(INITIAL_CAPACITY, false);
    let elem_size = element_byte_size(ctx, function, ListMethod::New)?;
    let alloc_size = ctx
        .builder
        .build_int_mul(initial_cap, elem_size, "alloc_sz")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let raw_ptr = ctx
        .call_basic(malloc, &[alloc_size.into()], "buf")?
        .into_pointer_value();
    let result = build_list_struct(ctx, raw_ptr, i64_ty.const_zero(), initial_cap)?;
    ret_struct(ctx, result)
}

fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    ret_basic(ctx, len.into())
}

fn emit_empty_q<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .or_ice()?;
    ret_basic(ctx, is_empty.into())
}

fn emit_from_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    // `List<T>` is the `ListLiteral<T>` carrier, so this is value-wise an
    // identity. But `self` is borrowed and the caller drops it, so the
    // result must own an independent buffer rather than alias `self`'s.
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let cloned = clone_list_value(ctx, function, llvm_function, ListMethod::FromList, self_val)?;
    ret_struct(ctx, cloned)
}

/// Deep-clone a `List` value into an independent buffer the caller owns
/// outright. Every intrinsic return obeys this contract: `self` is
/// borrowed and released by the caller, so handing back `self_val`
/// (or any struct sharing its buffer) would double-free at scope exit.
fn clone_list_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ListMethod,
    self_val: StructValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, method)?;
    let new_buf = copy_buffer(
        ctx,
        llvm_function,
        element(method, function)?,
        buf_ptr,
        len,
        len,
        elem_size,
        "clone_self",
    )?;
    build_list_struct(ctx, new_buf, len, len)
}

fn emit_append<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let item_val = nth_param(function, llvm_function, 1, "item")?;
    let item_val = acquire_value(ctx, element(ListMethod::Append, function)?, item_val)?;

    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Append)?;

    let new_len = ctx
        .builder
        .build_int_add(len, i64_ty.const_int(1, false), "new_len")
        .or_ice()?;
    let new_buf = copy_buffer(
        ctx,
        llvm_function,
        element(ListMethod::Append, function)?,
        buf_ptr,
        len,
        new_len,
        elem_size,
        "append",
    )?;

    let byte_offset = ctx
        .builder
        .build_int_mul(len, elem_size, "byte_off")
        .or_ice()?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, new_buf, &[byte_offset], "elem_ptr")
            .or_ice()?
    };
    ctx.builder.build_store(elem_ptr, item_val).or_ice()?;

    let result = build_list_struct(ctx, new_buf, new_len, new_len)?;
    ret_struct(ctx, result)
}

fn emit_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let option_symbol = expect_enum_symbol(&function.return_type, function, ListMethod::Get)?;
    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let oob_bb = ctx.context.append_basic_block(llvm_function, "oob");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let index = nth_int(function, llvm_function, 1, "index")?;

    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Get)?;
    let elem_ty = ir_basic_type(ctx, element(ListMethod::Get, function)?)?;

    let in_bounds = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, len, "in_bounds")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(in_bounds, ok_bb, oob_bb)
        .or_ice()?;

    ctx.builder.position_at_end(ok_bb);
    let byte_offset = ctx
        .builder
        .build_int_mul(index, elem_size, "byte_off")
        .or_ice()?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[byte_offset], "elem_ptr")
            .or_ice()?
    };
    let value = ctx
        .builder
        .build_load(elem_ty, elem_ptr, "elem_val")
        .or_ice()?;
    let value = acquire_value(ctx, element(ListMethod::Get, function)?, value)?;
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[value])?;
    ctx.builder.build_return(Some(&some)).or_ice().map(|_| ())?;

    ctx.builder.position_at_end(oob_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder.build_return(Some(&none)).or_ice().map(|_| ())?;

    let _ = entry;
    Ok(())
}

fn emit_pop<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let tuple_struct = ir_basic_type(ctx, &function.return_type)?.into_struct_type();
    let option_symbol = tuple_element_enum_symbol(&function.return_type, 0, function)?;

    let empty_bb = ctx.context.append_basic_block(llvm_function, "empty");
    let nonempty_bb = ctx.context.append_basic_block(llvm_function, "nonempty");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Pop)?;
    let elem_ty = ir_basic_type(ctx, element(ListMethod::Pop, function)?)?;

    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(is_empty, empty_bb, nonempty_bb)
        .or_ice()?;

    ctx.builder.position_at_end(empty_bb);
    let none = build_enum_value(ctx, &option_symbol, OPTION_NONE_TAG, &[])?;
    // Value semantics: the returned list must own an independent
    // buffer. Handing back `self_val` directly aliases the caller's
    // receiver slot, so both would free the same buffer at scope exit
    // (double free). Clone into a fresh buffer instead. `len` is zero
    // on this branch, so this allocates an empty buffer and copies
    // nothing, mirroring the nonempty branch's `copy_buffer`.
    let empty_buf = copy_buffer(
        ctx,
        llvm_function,
        element(ListMethod::Pop, function)?,
        buf_ptr,
        len,
        len,
        elem_size,
        "pop_empty",
    )?;
    let empty_list = build_list_struct(ctx, empty_buf, len, len)?;
    let tuple_empty = build_tuple(ctx, tuple_struct, none, empty_list.into())?;
    ctx.builder
        .build_return(Some(&tuple_empty))
        .or_ice()
        .map(|_| ())?;

    ctx.builder.position_at_end(nonempty_bb);
    let new_len = ctx
        .builder
        .build_int_sub(len, i64_ty.const_int(1, false), "new_len")
        .or_ice()?;
    let byte_offset = ctx
        .builder
        .build_int_mul(new_len, elem_size, "byte_off")
        .or_ice()?;
    // The popped element lives at `new_len` in the original buffer
    // (it's excluded from the copy below).
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[byte_offset], "elem_ptr")
            .or_ice()?
    };
    let elem_val = ctx
        .builder
        .build_load(elem_ty, elem_ptr, "elem_val")
        .or_ice()?;
    let elem_val = acquire_value(ctx, element(ListMethod::Pop, function)?, elem_val)?;
    let some = build_enum_value(ctx, &option_symbol, OPTION_SOME_TAG, &[elem_val])?;
    let new_buf = copy_buffer(
        ctx,
        llvm_function,
        element(ListMethod::Pop, function)?,
        buf_ptr,
        new_len,
        new_len,
        elem_size,
        "pop",
    )?;
    let shortened = build_list_struct(ctx, new_buf, new_len, new_len)?;
    let tuple_nonempty = build_tuple(ctx, tuple_struct, some, shortened.into())?;
    ctx.builder
        .build_return(Some(&tuple_nonempty))
        .or_ice()
        .map(|_| ())?;

    let _ = entry;
    Ok(())
}

fn emit_replace_at<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
) -> Result<(), LlvmError> {
    let in_bounds_bb = ctx.context.append_basic_block(llvm_function, "in_bounds");
    let done_bb = ctx.context.append_basic_block(llvm_function, "done");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let index = nth_int(function, llvm_function, 1, "index")?;
    let value = nth_param(function, llvm_function, 2, "value")?;

    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::ReplaceAt)?;

    let in_bounds = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, len, "in_bounds")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(in_bounds, in_bounds_bb, done_bb)
        .or_ice()?;

    ctx.builder.position_at_end(in_bounds_bb);
    let elem_ty = element(ListMethod::ReplaceAt, function)?;
    let new_buf = copy_buffer(
        ctx,
        llvm_function,
        elem_ty,
        buf_ptr,
        len,
        len,
        elem_size,
        "replace",
    )?;
    let elem_ptr = element_slot(ctx, new_buf, index, elem_size)?;
    // `copy_buffer` acquired every retained element, including the one
    // at `index` we're about to overwrite. Release that copy so the
    // incoming value (acquired next) is the slot's sole owner.
    release_in_slot(ctx, elem_ty, elem_ptr)?;
    let value = acquire_value(ctx, elem_ty, value)?;
    ctx.builder.build_store(elem_ptr, value).or_ice()?;
    let replaced = build_list_struct(ctx, new_buf, len, len)?;
    ret_struct(ctx, replaced)?;

    // Out of bounds: no element changes, but `self` is borrowed and the
    // caller drops it, so the result must own an independent buffer
    // rather than alias `self`'s (which would double-free at scope
    // exit). Clone the unchanged list, mirroring the in-bounds path.
    ctx.builder.position_at_end(done_bb);
    let unchanged = clone_list_value(
        ctx,
        function,
        llvm_function,
        ListMethod::ReplaceAt,
        self_val,
    )?;
    ret_struct(ctx, unchanged)?;

    let _ = entry;
    Ok(())
}

fn emit_slice<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let nonempty_bb = ctx.context.append_basic_block(llvm_function, "nonempty");
    let empty_bb = ctx.context.append_basic_block(llvm_function, "empty");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let start = nth_int(function, llvm_function, 1, "start")?;
    let count = nth_int(function, llvm_function, 2, "count")?;

    let buf_ptr = build_extract_pointer(ctx, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Slice)?;

    // Clamp start: if start >= len, clamped_start = len.
    let start_ok = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, start, len, "start_ok")
        .or_ice()?;
    let clamped_start = ctx
        .builder
        .build_select(start_ok, start, len, "clamped_start")
        .or_ice()?
        .into_int_value();

    let remaining = ctx
        .builder
        .build_int_sub(len, clamped_start, "remaining")
        .or_ice()?;
    let count_ok = ctx
        .builder
        .build_int_compare(IntPredicate::ULE, count, remaining, "count_ok")
        .or_ice()?;
    let clamped_count = ctx
        .builder
        .build_select(count_ok, count, remaining, "clamped_count")
        .or_ice()?
        .into_int_value();

    let has_elems = ctx
        .builder
        .build_int_compare(
            IntPredicate::UGT,
            clamped_count,
            i64_ty.const_zero(),
            "has_elems",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(has_elems, nonempty_bb, empty_bb)
        .or_ice()?;

    ctx.builder.position_at_end(nonempty_bb);
    let alloc_bytes = ctx
        .builder
        .build_int_mul(clamped_count, elem_size, "alloc_bytes")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let new_buf = ctx
        .call_basic(malloc, &[alloc_bytes.into()], "new_buf")?
        .into_pointer_value();
    let src_offset = ctx
        .builder
        .build_int_mul(clamped_start, elem_size, "src_off")
        .or_ice()?;
    let src_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[src_offset], "src_ptr")
            .or_ice()?
    };
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[new_buf.into(), src_ptr.into(), alloc_bytes.into()],
            "",
        )
        .or_ice()?;
    acquire_buffer(
        ctx,
        llvm_function,
        element(ListMethod::Slice, function)?,
        new_buf,
        clamped_count,
        elem_size,
        "slice",
    )?;
    let nonempty_result = build_list_struct(ctx, new_buf, clamped_count, clamped_count)?;
    ret_struct(ctx, nonempty_result)?;

    ctx.builder.position_at_end(empty_bb);
    let empty_buf = ctx
        .call_basic(malloc, &[i64_ty.const_zero().into()], "empty_buf")?
        .into_pointer_value();
    let empty_result = build_list_struct(ctx, empty_buf, i64_ty.const_zero(), i64_ty.const_zero())?;
    ret_struct(ctx, empty_result)?;

    let _ = entry;
    Ok(())
}

fn emit_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let other_val = nth_list(function, llvm_function, 1, "other")?;

    let self_ptr = build_extract_pointer(ctx, self_val, 0, "self_ptr")?;
    let self_len = build_extract_int(ctx, self_val, 1, "self_len")?;
    let other_ptr = build_extract_pointer(ctx, other_val, 0, "other_ptr")?;
    let other_len = build_extract_int(ctx, other_val, 1, "other_len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Concat)?;

    let total_len = ctx
        .builder
        .build_int_add(self_len, other_len, "total_len")
        .or_ice()?;

    // Copy-on-write: a fresh `total_len` buffer seeded with `self`'s
    // elements, then `other` appended after them. Neither input buffer
    // is mutated.
    let elem_ty = element(ListMethod::Concat, function)?;
    let new_buf = copy_buffer(
        ctx,
        llvm_function,
        elem_ty,
        self_ptr,
        self_len,
        total_len,
        elem_size,
        "concat",
    )?;
    let dst_offset = ctx
        .builder
        .build_int_mul(self_len, elem_size, "dst_off")
        .or_ice()?;
    let dst_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, new_buf, &[dst_offset], "dst_ptr")
            .or_ice()?
    };
    let copy_bytes = ctx
        .builder
        .build_int_mul(other_len, elem_size, "copy_bytes")
        .or_ice()?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[dst_ptr.into(), other_ptr.into(), copy_bytes.into()],
            "",
        )
        .or_ice()?;
    // `copy_buffer` acquired `self`'s half. The `other` half was raw
    // `memcpy`'d above, so acquire it too, giving the result
    // independent references to every element.
    acquire_buffer(
        ctx,
        llvm_function,
        elem_ty,
        dst_ptr,
        other_len,
        elem_size,
        "concat.other",
    )?;

    let result = build_list_struct(ctx, new_buf, total_len, total_len)?;
    ret_struct(ctx, result)
}

// --- helpers --------------------------------------------------------------

/// Allocate a fresh `new_cap`-capacity buffer, copy the first
/// `copy_count` elements out of `src`, then *acquire* each copy so the
/// new buffer owns independent references. Under value semantics every
/// list mutator is copy-on-write: it builds a new buffer instead of
/// touching `src`, so a binding shared by assignment is never
/// observably changed through another alias, and balancing the
/// refcount here is what stops the shared payloads from double-freeing
/// once both the source and the copy are reclaimed by drop glue.
#[allow(clippy::too_many_arguments)]
fn copy_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    element: &IRType,
    src: PointerValue<'ctx>,
    copy_count: IntValue<'ctx>,
    new_cap: IntValue<'ctx>,
    elem_size: IntValue<'ctx>,
    label: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let alloc_bytes = ctx
        .builder
        .build_int_mul(new_cap, elem_size, "alloc_bytes")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let new_buf = ctx
        .call_basic(malloc, &[alloc_bytes.into()], "new_buf")?
        .into_pointer_value();
    let copy_bytes = ctx
        .builder
        .build_int_mul(copy_count, elem_size, "copy_bytes")
        .or_ice()?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(memcpy, &[new_buf.into(), src.into(), copy_bytes.into()], "")
        .or_ice()?;
    acquire_buffer(
        ctx,
        llvm_function,
        element,
        new_buf,
        copy_count,
        elem_size,
        label,
    )?;
    Ok(new_buf)
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

fn nth_int<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::IntValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected integer for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn nth_list<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<StructValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::StructValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected list struct for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn build_extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    list: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = ctx
        .builder
        .build_extract_value(list, index, name)
        .or_ice()?;
    Ok(raw.into_int_value())
}

fn build_extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    list: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = ctx
        .builder
        .build_extract_value(list, index, name)
        .or_ice()?;
    Ok(raw.into_pointer_value())
}

fn build_list_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let undef = list_ty.get_undef();
    let with_buf = ctx
        .builder
        .build_insert_value(undef, buf, 0, "with_buf")
        .or_ice()?
        .into_struct_value();
    let with_len = ctx
        .builder
        .build_insert_value(with_buf, len, 1, "with_len")
        .or_ice()?
        .into_struct_value();
    let with_cap = ctx
        .builder
        .build_insert_value(with_len, cap, 2, "with_cap")
        .or_ice()?
        .into_struct_value();
    Ok(with_cap)
}

/// Extract the IR symbol of an enum type from `ty`, surfacing a
/// codegen-error (not a panic) if the slot turns out to be anything
/// else. The intrinsic emitter only calls this when the lowering
/// pass guarantees an enum-typed slot. The error is a defensive
/// last line in case lowering / IR-seal invariants slip.
fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
    method: ListMethod,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "List.{method:?} expected an enum-typed slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

/// Resolve the enum symbol stored at `element_index` of a tuple.
/// `List.pop` uses this to recover its concrete `Option<T>`.
fn tuple_element_enum_symbol(
    tuple_ty: &IRType,
    element_index: usize,
    function: &IRFunction,
) -> Result<IRSymbol, LlvmError> {
    let IRType::Tuple(elements) = tuple_ty else {
        return Err(LlvmError::Codegen(format!(
            "List.pop expected a tuple return type, got `{tuple_ty:?}` (symbol `{}`)",
            function.symbol,
        )));
    };
    match elements.get(element_index) {
        Some(IRType::Enum(symbol)) => Ok(symbol.clone()),
        other => Err(LlvmError::Codegen(format!(
            "List.pop expected an enum-typed element at index {element_index}, \
             got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

fn build_tuple<'ctx>(
    ctx: &EmitContext<'ctx>,
    tuple_struct: StructType<'ctx>,
    first_element: BasicValueEnum<'ctx>,
    second_element: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let alloca = ctx
        .builder
        .build_alloca(tuple_struct, "tuple_alloca")
        .or_ice()?;
    let first_ptr = ctx
        .builder
        .build_struct_gep(tuple_struct, alloca, 0, "first_ptr")
        .or_ice()?;
    ctx.builder.build_store(first_ptr, first_element).or_ice()?;
    let second_ptr = ctx
        .builder
        .build_struct_gep(tuple_struct, alloca, 1, "second_ptr")
        .or_ice()?;
    ctx.builder
        .build_store(second_ptr, second_element)
        .or_ice()?;
    ctx.builder
        .build_load(tuple_struct, alloca, "tuple_val")
        .or_ice()
}

fn ret_struct<'ctx>(ctx: &EmitContext<'ctx>, value: StructValue<'ctx>) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn ret_basic<'ctx>(ctx: &EmitContext<'ctx>, value: BasicValueEnum<'ctx>) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}
