//! `List<T>` family — heap-backed dynamic array. Layout is
//! `{ buf_ptr: i8*, len: i64, cap: i64 }`, passed by value; element
//! storage lives off-heap behind `buf_ptr`. Methods malloc / realloc
//! / memcpy via libc directly — no Rust-side runtime helpers.
//!
//! Element-size-parameterized: each method computes `elem_size` from
//! the `IRType::List(_)` inner type carried on the function's
//! signature, then generates the same shape of IR regardless of `T`.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicType, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, ListMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_malloc_extern, declare_realloc_extern};
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
/// it on the return type; every other method has `self: List<T>` as
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
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let raw_ptr = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "buf")
        .map_err(|e| {
            inkwell_err(
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
        })?
        .into_pointer_value();
    let result = build_list_struct(ctx, function, raw_ptr, i64_ty.const_zero(), initial_cap)?;
    ret_struct(ctx, function, result)
}

fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    ret_basic(ctx, function, len.into())
}

fn emit_empty_q<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ret_basic(ctx, function, is_empty.into())
}

fn emit_from_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    // Identity: `List<T>` is the default `ListLiteral<T>` carrier.
    let self_val = nth_list(function, llvm_function, 0, "self")?;
    ret_struct(ctx, function, self_val)
}

fn emit_append<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let entry = ctx.builder.get_insert_block().unwrap();
    let grow_bb = ctx.context.append_basic_block(llvm_function, "grow");
    let store_bb = ctx.context.append_basic_block(llvm_function, "store");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let item_val = nth_param(function, llvm_function, 1, "item")?;

    let buf_ptr = build_extract_pointer(ctx, function, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let cap = build_extract_int(ctx, function, self_val, 2, "cap")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Append)?;

    let needs_grow = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, cap, "needs_grow")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(needs_grow, grow_bb, store_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(grow_bb);
    let new_cap = ctx
        .builder
        .build_int_mul(cap, i64_ty.const_int(2, false), "new_cap")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let new_size = ctx
        .builder
        .build_int_mul(new_cap, elem_size, "new_size")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let realloc = declare_realloc_extern(ctx);
    let new_ptr = ctx
        .builder
        .build_call(realloc, &[buf_ptr.into(), new_size.into()], "new_buf")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call realloc for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "realloc returned no value for `{}`",
                function.symbol,
            ))
        })?
        .into_pointer_value();
    ctx.builder
        .build_unconditional_branch(store_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_unconditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(store_bb);
    let ptr_phi = ctx
        .builder
        .build_phi(ctx.context.ptr_type(AddressSpace::default()), "ptr_phi")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    ptr_phi.add_incoming(&[(&buf_ptr, entry), (&new_ptr, grow_bb)]);
    let cap_phi = ctx
        .builder
        .build_phi(i64_ty, "cap_phi")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    cap_phi.add_incoming(&[(&cap, entry), (&new_cap, grow_bb)]);

    let final_ptr = ptr_phi.as_basic_value().into_pointer_value();
    let final_cap = cap_phi.as_basic_value().into_int_value();

    let byte_offset = ctx
        .builder
        .build_int_mul(len, elem_size, "byte_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, final_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(elem_ptr, item_val)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;

    let new_len = ctx
        .builder
        .build_int_add(len, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let result = build_list_struct(ctx, function, final_ptr, new_len, final_cap)?;
    ret_struct(ctx, function, result)
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

    let buf_ptr = build_extract_pointer(ctx, function, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Get)?;
    let elem_ty = ir_basic_type(ctx, element(ListMethod::Get, function)?)?;

    let in_bounds = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, len, "in_bounds")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(in_bounds, ok_bb, oob_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(ok_bb);
    let byte_offset = ctx
        .builder
        .build_int_mul(index, elem_size, "byte_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let value = ctx
        .builder
        .build_load(elem_ty, elem_ptr, "elem_val")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[value])?;
    ctx.builder
        .build_return(Some(&some))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(oob_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder
        .build_return(Some(&none))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))?;

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
    let pair_struct = ir_basic_type(ctx, &function.return_type)?.into_struct_type();
    let option_symbol = struct_field_enum_symbol(ctx, &function.return_type, 0, function)?;

    let empty_bb = ctx.context.append_basic_block(llvm_function, "empty");
    let nonempty_bb = ctx.context.append_basic_block(llvm_function, "nonempty");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let buf_ptr = build_extract_pointer(ctx, function, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let cap = build_extract_int(ctx, function, self_val, 2, "cap")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Pop)?;
    let elem_ty = ir_basic_type(ctx, element(ListMethod::Pop, function)?)?;

    let is_empty = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, len, i64_ty.const_zero(), "is_empty")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(is_empty, empty_bb, nonempty_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(empty_bb);
    let none = build_enum_value(ctx, &option_symbol, OPTION_NONE_TAG, &[])?;
    let pair_empty = build_pair(ctx, function, pair_struct, none, self_val.into())?;
    ctx.builder
        .build_return(Some(&pair_empty))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(nonempty_bb);
    let new_len = ctx
        .builder
        .build_int_sub(len, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| inkwell_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let byte_offset = ctx
        .builder
        .build_int_mul(new_len, elem_size, "byte_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let elem_val = ctx
        .builder
        .build_load(elem_ty, elem_ptr, "elem_val")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let some = build_enum_value(ctx, &option_symbol, OPTION_SOME_TAG, &[elem_val])?;
    let shortened = build_list_struct(ctx, function, buf_ptr, new_len, cap)?;
    let pair_nonempty = build_pair(ctx, function, pair_struct, some, shortened.into())?;
    ctx.builder
        .build_return(Some(&pair_nonempty))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))?;

    let _ = entry;
    Ok(())
}

fn emit_replace_at<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    entry: BasicBlock<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let in_bounds_bb = ctx.context.append_basic_block(llvm_function, "in_bounds");
    let done_bb = ctx.context.append_basic_block(llvm_function, "done");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let index = nth_int(function, llvm_function, 1, "index")?;
    let value = nth_param(function, llvm_function, 2, "value")?;

    let buf_ptr = build_extract_pointer(ctx, function, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::ReplaceAt)?;

    let in_bounds = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, len, "in_bounds")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(in_bounds, in_bounds_bb, done_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(in_bounds_bb);
    let byte_offset = ctx
        .builder
        .build_int_mul(index, elem_size, "byte_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(elem_ptr, value)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_unconditional_branch(done_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_unconditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(done_bb);
    ret_struct(ctx, function, self_val)?;

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

    let buf_ptr = build_extract_pointer(ctx, function, self_val, 0, "buf_ptr")?;
    let len = build_extract_int(ctx, function, self_val, 1, "len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Slice)?;

    // Clamp start: if start >= len, clamped_start = len.
    let start_ok = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, start, len, "start_ok")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    let clamped_start = ctx
        .builder
        .build_select(start_ok, start, len, "clamped_start")
        .map_err(|e| inkwell_err(format_args!("build_select for `{}`", function.symbol), e))?
        .into_int_value();

    let remaining = ctx
        .builder
        .build_int_sub(len, clamped_start, "remaining")
        .map_err(|e| inkwell_err(format_args!("build_int_sub for `{}`", function.symbol), e))?;
    let count_ok = ctx
        .builder
        .build_int_compare(IntPredicate::ULE, count, remaining, "count_ok")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    let clamped_count = ctx
        .builder
        .build_select(count_ok, count, remaining, "clamped_count")
        .map_err(|e| inkwell_err(format_args!("build_select for `{}`", function.symbol), e))?
        .into_int_value();

    let has_elems = ctx
        .builder
        .build_int_compare(
            IntPredicate::UGT,
            clamped_count,
            i64_ty.const_zero(),
            "has_elems",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(has_elems, nonempty_bb, empty_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(nonempty_bb);
    let alloc_bytes = ctx
        .builder
        .build_int_mul(clamped_count, elem_size, "alloc_bytes")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let new_buf = ctx
        .builder
        .build_call(malloc, &[alloc_bytes.into()], "new_buf")
        .map_err(|e| {
            inkwell_err(
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
        })?
        .into_pointer_value();
    let src_offset = ctx
        .builder
        .build_int_mul(clamped_start, elem_size, "src_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let src_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, buf_ptr, &[src_offset], "src_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[new_buf.into(), src_ptr.into(), alloc_bytes.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call memcpy for `{}`", function.symbol),
                e,
            )
        })?;
    let nonempty_result = build_list_struct(ctx, function, new_buf, clamped_count, clamped_count)?;
    ret_struct(ctx, function, nonempty_result)?;

    ctx.builder.position_at_end(empty_bb);
    let empty_buf = ctx
        .builder
        .build_call(malloc, &[i64_ty.const_zero().into()], "empty_buf")
        .map_err(|e| {
            inkwell_err(
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
        })?
        .into_pointer_value();
    let empty_result = build_list_struct(
        ctx,
        function,
        empty_buf,
        i64_ty.const_zero(),
        i64_ty.const_zero(),
    )?;
    ret_struct(ctx, function, empty_result)?;

    let _ = entry;
    Ok(())
}

fn emit_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry = ctx.builder.get_insert_block().unwrap();
    let grow_bb = ctx.context.append_basic_block(llvm_function, "grow");
    let copy_bb = ctx.context.append_basic_block(llvm_function, "copy");

    let self_val = nth_list(function, llvm_function, 0, "self")?;
    let other_val = nth_list(function, llvm_function, 1, "other")?;

    let self_ptr = build_extract_pointer(ctx, function, self_val, 0, "self_ptr")?;
    let self_len = build_extract_int(ctx, function, self_val, 1, "self_len")?;
    let self_cap = build_extract_int(ctx, function, self_val, 2, "self_cap")?;
    let other_ptr = build_extract_pointer(ctx, function, other_val, 0, "other_ptr")?;
    let other_len = build_extract_int(ctx, function, other_val, 1, "other_len")?;
    let elem_size = element_byte_size(ctx, function, ListMethod::Concat)?;

    let total_len = ctx
        .builder
        .build_int_add(self_len, other_len, "total_len")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let needs_grow = ctx
        .builder
        .build_int_compare(IntPredicate::UGT, total_len, self_cap, "needs_grow")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(needs_grow, grow_bb, copy_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(grow_bb);
    let new_cap = total_len;
    let new_size = ctx
        .builder
        .build_int_mul(new_cap, elem_size, "new_size")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let realloc = declare_realloc_extern(ctx);
    let new_ptr = ctx
        .builder
        .build_call(realloc, &[self_ptr.into(), new_size.into()], "new_buf")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call realloc for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "realloc returned no value for `{}`",
                function.symbol,
            ))
        })?
        .into_pointer_value();
    ctx.builder
        .build_unconditional_branch(copy_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_unconditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(copy_bb);
    let ptr_phi = ctx
        .builder
        .build_phi(ctx.context.ptr_type(AddressSpace::default()), "ptr_phi")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    ptr_phi.add_incoming(&[(&self_ptr, entry), (&new_ptr, grow_bb)]);
    let cap_phi = ctx
        .builder
        .build_phi(i64_ty, "cap_phi")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    cap_phi.add_incoming(&[(&self_cap, entry), (&new_cap, grow_bb)]);

    let final_ptr = ptr_phi.as_basic_value().into_pointer_value();
    let final_cap = cap_phi.as_basic_value().into_int_value();

    let dst_offset = ctx
        .builder
        .build_int_mul(self_len, elem_size, "dst_off")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let dst_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, final_ptr, &[dst_offset], "dst_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let copy_bytes = ctx
        .builder
        .build_int_mul(other_len, elem_size, "copy_bytes")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[dst_ptr.into(), other_ptr.into(), copy_bytes.into()],
            "",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call memcpy for `{}`", function.symbol),
                e,
            )
        })?;

    let result = build_list_struct(ctx, function, final_ptr, total_len, final_cap)?;
    ret_struct(ctx, function, result)
}

// --- helpers --------------------------------------------------------------

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
    function: &IRFunction,
    list: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = ctx
        .builder
        .build_extract_value(list, index, name)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?;
    Ok(raw.into_int_value())
}

fn build_extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    list: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = ctx
        .builder
        .build_extract_value(list, index, name)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?;
    Ok(raw.into_pointer_value())
}

fn build_list_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let undef = list_ty.get_undef();
    let with_buf = ctx
        .builder
        .build_insert_value(undef, buf, 0, "with_buf")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let with_len = ctx
        .builder
        .build_insert_value(with_buf, len, 1, "with_len")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    let with_cap = ctx
        .builder
        .build_insert_value(with_len, cap, 2, "with_cap")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_insert_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_struct_value();
    Ok(with_cap)
}

/// Extract the IR symbol of an enum type from `ty`, surfacing a
/// codegen-error (not a panic) if the slot turns out to be anything
/// else. The intrinsic emitter only calls this when the lowering
/// pass guarantees an enum-typed slot — the error is a defensive
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

/// Resolve the enum symbol stored at `field_index` of the struct
/// referenced by `struct_ty`. Used by `List.pop` to recover the
/// inner `Option<T>` symbol from its `Pair<Option<T>, List<T>>`
/// return type — without this step, the intrinsic would have to
/// re-derive the symbol from mangled names, duplicating the
/// lowering pass's mangling rules.
fn struct_field_enum_symbol<'ctx>(
    ctx: &EmitContext<'ctx>,
    struct_ty: &IRType,
    field_index: usize,
    function: &IRFunction,
) -> Result<IRSymbol, LlvmError> {
    let IRType::Struct(struct_symbol) = struct_ty else {
        return Err(LlvmError::Codegen(format!(
            "List.pop expected a struct return type, got `{struct_ty:?}` (symbol `{}`)",
            function.symbol,
        )));
    };
    let field_ty = ctx.layouts.struct_field_ir_type(struct_symbol, field_index);
    match field_ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "List.pop expected an enum-typed field at index {field_index} of `{struct_symbol}`, \
             got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

fn build_pair<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    pair_struct: StructType<'ctx>,
    first: BasicValueEnum<'ctx>,
    second: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let alloca = ctx
        .builder
        .build_alloca(pair_struct, "pair_alloca")
        .map_err(|e| inkwell_err(format_args!("build_alloca for `{}`", function.symbol), e))?;
    let first_ptr = ctx
        .builder
        .build_struct_gep(pair_struct, alloca, 0, "first_ptr")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_store(first_ptr, first)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let second_ptr = ctx
        .builder
        .build_struct_gep(pair_struct, alloca, 1, "second_ptr")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_store(second_ptr, second)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_load(pair_struct, alloca, "pair_val")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))
}

fn ret_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: StructValue<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn ret_basic<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}
