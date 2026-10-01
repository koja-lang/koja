//! `String` method intrinsics. Trivial cells inline against the
//! [`crate::emit::heap_layout`] block, and the walking cells
//! (`length`, `get`, `slice`, `find`, `slice_bytes`) delegate to
//! `koja-runtime` helpers so unicode walking and byte search stay in
//! Rust.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol, IRType, StringMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::heap_layout::{Rounding, load_byte_count};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::heap_payload;
use crate::intrinsics::option;
use crate::intrinsics::result;
use crate::intrinsics::util::{expect_enum_symbol, nth_param, nth_pointer, nth_struct};
use crate::runtime::{
    declare_malloc_extern, declare_memcpy_extern, declare_string_contains_nul_extern,
    declare_string_find_extern, declare_string_get_extern, declare_string_length_extern,
    declare_string_next_extern, declare_string_slice_bytes_extern, declare_string_slice_extern,
};
use crate::types::{ir_basic_type, tuple_struct_type};

pub(super) fn emit_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: StringMethod,
) -> Result<(), LlvmError> {
    match method {
        StringMethod::ByteLength => emit_byte_length(ctx, function, llvm_function),
        StringMethod::Find => super::binary::emit_find(
            ctx,
            function,
            llvm_function,
            declare_string_find_extern(ctx),
        ),
        StringMethod::Get => emit_get(ctx, function, llvm_function),
        StringMethod::Length => emit_length(ctx, function, llvm_function),
        StringMethod::Next => emit_next(ctx, function, llvm_function),
        StringMethod::Slice => emit_slice(ctx, function, llvm_function),
        StringMethod::SliceBytes => emit_slice_bytes(ctx, function, llvm_function),
        StringMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
        StringMethod::ToCstring => emit_to_cstring(ctx, function, llvm_function),
    }
}

/// Emits `String.slice_bytes(self, start, stop)` as a straight call
/// into the `koja_string_slice_bytes` runtime helper with the two byte
/// offsets.
fn emit_slice_bytes<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let start = nth_param(function, llvm_function, 1, "start");
    let stop = nth_param(function, llvm_function, 2, "stop");
    let helper = declare_string_slice_bytes_extern(ctx);
    let value = ctx.call_basic(
        helper,
        &[payload.into(), start.into(), stop.into()],
        "sliced",
    )?;
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn emit_byte_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let byte_count = load_byte_count(ctx, payload, Rounding::Floor)?;
    ctx.builder
        .build_return(Some(&byte_count))
        .or_ice()
        .map(|_| ())
}

/// `String.to_binary(self) -> Binary`: a zero-cost reinterpret.
/// `String` and `Binary` share the `[rc][bit_length][bytes]` block
/// (the String's trailing libc NUL is just unused capacity a `Binary`
/// never reads), so we rc-acquire the immutable block and hand back
/// the same payload pointer as an owned `Binary`. The matching `Drop`
/// rc-decrements either alias.
fn emit_to_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let shared = heap_payload::share_heap_payload(ctx, function.symbol.mangled(), payload)?;
    ctx.builder.build_return(Some(&shared)).or_ice().map(|_| ())
}

fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let helper = declare_string_length_extern(ctx);
    let value = ctx.call_basic(helper, &[payload.into()], "len")?;
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn emit_slice<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let range_struct = nth_struct(function, llvm_function, 1, "range");
    let start = ctx
        .builder
        .build_extract_value(range_struct, 0, "start")
        .or_ice()?;
    let stop = ctx
        .builder
        .build_extract_value(range_struct, 1, "stop")
        .or_ice()?;
    let helper = declare_string_slice_extern(ctx);
    let value = ctx.call_basic(
        helper,
        &[payload.into(), start.into(), stop.into()],
        "sliced",
    )?;
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn emit_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function);
    let index = nth_param(function, llvm_function, 1, "index");
    let helper = declare_string_get_extern(ctx);
    let raw_ptr = ctx
        .call_basic(helper, &[payload.into(), index.into()], "ch")?
        .into_pointer_value();

    let option_symbol = expect_enum_symbol(&function.return_type, function, "String.get");
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let is_null = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, raw_ptr, ptr_ty.const_null(), "is_null")
        .or_ice()?;

    let some_bb = ctx.context.append_basic_block(llvm_function, "some");
    let none_bb = ctx.context.append_basic_block(llvm_function, "none");
    ctx.builder
        .build_conditional_branch(is_null, none_bb, some_bb)
        .or_ice()?;

    ctx.builder.position_at_end(some_bb);
    let some = build_enum_value(
        ctx,
        option_symbol,
        option::some_tag(ctx, option_symbol),
        &[raw_ptr.into()],
    )?;
    ctx.builder.build_return(Some(&some)).or_ice()?;

    ctx.builder.position_at_end(none_bb);
    let none = build_enum_value(
        ctx,
        option_symbol,
        option::none_tag(ctx, option_symbol),
        &[],
    )?;
    ctx.builder.build_return(Some(&none)).or_ice().map(|_| ())
}

fn emit_next<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let i64_ty = ctx.context.i64_type();
    let payload = self_payload(function, llvm_function);
    let cursor = nth_param(function, llvm_function, 1, "cursor");
    let next_cursor = ctx.builder.build_alloca(i64_ty, "next_cursor").or_ice()?;
    let helper = declare_string_next_extern(ctx);
    let character = ctx
        .call_basic(
            helper,
            &[payload.into(), cursor.into(), next_cursor.into()],
            "character",
        )?
        .into_pointer_value();
    let is_none = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, character, ptr_ty.const_null(), "is_none")
        .or_ice()?;
    let some_bb = ctx.context.append_basic_block(llvm_function, "some");
    let none_bb = ctx.context.append_basic_block(llvm_function, "none");
    ctx.builder
        .build_conditional_branch(is_none, none_bb, some_bb)
        .or_ice()?;

    let option_symbol = expect_enum_symbol(&function.return_type, function, "String.next");
    ctx.builder.position_at_end(some_bb);
    let next = ctx
        .builder
        .build_load(i64_ty, next_cursor, "next")
        .or_ice()?;
    let tuple_ty = tuple_struct_type(ctx, &[IRType::String, IRType::Int64])?;
    let tuple = ctx
        .builder
        .build_insert_value(tuple_ty.get_undef(), character, 0, "with_character")
        .or_ice()?
        .into_struct_value();
    let tuple = ctx
        .builder
        .build_insert_value(tuple, next, 1, "with_next")
        .or_ice()?
        .into_struct_value();
    let some = build_enum_value(
        ctx,
        option_symbol,
        option::some_tag(ctx, option_symbol),
        &[tuple.into()],
    )?;
    ctx.builder.build_return(Some(&some)).or_ice()?;

    ctx.builder.position_at_end(none_bb);
    let none = build_enum_value(
        ctx,
        option_symbol,
        option::none_tag(ctx, option_symbol),
        &[],
    )?;
    ctx.builder.build_return(Some(&none)).or_ice().map(|_| ())
}

fn emit_to_cstring<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let payload = self_payload(function, llvm_function);
    let byte_len = load_byte_count(ctx, payload, Rounding::Floor)?;
    let result_symbol = result::return_symbol(function);
    let cstring_ty = cstring_struct_type(ctx, result_symbol)?;

    let contains_nul = declare_string_contains_nul_extern(ctx);
    let has_nul = ctx
        .call_basic(contains_nul, &[payload.into()], "has_nul")?
        .into_int_value();
    let rejected = ctx
        .builder
        .build_int_compare(IntPredicate::NE, has_nul, i64_ty.const_zero(), "rejected")
        .or_ice()?;
    let invalid_bb = ctx
        .context
        .append_basic_block(llvm_function, "interior_nul");
    let valid_bb = ctx.context.append_basic_block(llvm_function, "valid");
    ctx.builder
        .build_conditional_branch(rejected, invalid_bb, valid_bb)
        .or_ice()?;

    ctx.builder.position_at_end(invalid_bb);
    let error = result::build_unit_error(ctx, result_symbol, "InteriorNul")?;
    ctx.builder.build_return(Some(&error)).or_ice()?;

    ctx.builder.position_at_end(valid_bb);
    emit_cstring_success(ctx, result_symbol, cstring_ty, payload, byte_len)
}

fn cstring_struct_type<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<StructType<'ctx>, LlvmError> {
    let cstring_type =
        result::single_payload_type(ctx, result_symbol, result::ok_tag(ctx, result_symbol));
    match cstring_type {
        IRType::Struct(_) => Ok(ir_basic_type(ctx, &cstring_type)?.into_struct_type()),
        other => panic!("String.to_cstring expected a CString Ok payload, got `{other:?}`"),
    }
}

fn emit_cstring_success<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    cstring_ty: StructType<'ctx>,
    payload: PointerValue<'ctx>,
    byte_len: IntValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let alloc_size = ctx
        .builder
        .build_int_add(byte_len, i64_ty.const_int(1, false), "alloc_size")
        .or_ice()?;
    let malloc = declare_malloc_extern(ctx);
    let buf = ctx
        .call_basic(malloc, &[alloc_size.into()], "c_buf")?
        .into_pointer_value();

    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(memcpy, &[buf.into(), payload.into(), byte_len.into()], "")
        .or_ice()?;
    // SAFETY: `alloc_size` is `byte_len + 1`, so the NUL slot is
    // inside `buf`.
    let nul_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, buf, &[byte_len], "nul")
            .or_ice()?
    };
    ctx.builder
        .build_store(nul_ptr, i8_ty.const_zero())
        .or_ice()?;

    let cstring = build_cstring(ctx, cstring_ty, buf, byte_len)?;
    let ok = result::build_ok(ctx, result_symbol, cstring)?;
    ctx.builder.build_return(Some(&ok)).or_ice().map(|_| ())
}

fn self_payload<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> PointerValue<'ctx> {
    nth_pointer(function, llvm_function, 0, "self")
}

fn build_cstring<'ctx>(
    ctx: &EmitContext<'ctx>,
    cstring_ty: StructType<'ctx>,
    ptr: PointerValue<'ctx>,
    len: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let with_ptr = ctx
        .builder
        .build_insert_value(cstring_ty.get_undef(), ptr, 0, "cs_ptr")
        .or_ice()?;
    ctx.builder
        .build_insert_value(with_ptr, len, 1, "cs_val")
        .or_ice()
        .map(|v| v.into_struct_value().into())
}
