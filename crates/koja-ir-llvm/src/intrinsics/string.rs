//! `String` method intrinsics. Mirrors v1's
//! `koja-codegen::intrinsics::string` + `cptr::emit_cstring_intrinsic`
//! port shape: trivial cells inline against the `[i64 bit_length]
//! [payload bytes]` layout (with the SSA pointer pointing at the
//! payload), and the codepoint-aware cells (`length`, `get`,
//! `slice`) delegate to `koja-runtime` helpers so unicode walking
//! stays in Rust.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, StringMethod};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::heap_layout::load_bit_length;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::heap_payload;
use crate::intrinsics::result;
use crate::runtime::{
    declare_malloc_extern, declare_string_contains_nul_extern, declare_string_get_extern,
    declare_string_length_extern, declare_string_slice_extern,
};
use crate::types::ir_basic_type;

/// `Option<T>` variant tags, matching the stdlib decl order: `Some`
/// first (tag 0), then `None`.
const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

pub(super) fn emit_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: StringMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    match method {
        StringMethod::ByteLength => emit_byte_length(ctx, function, llvm_function),
        StringMethod::Get => emit_get(ctx, function, llvm_function),
        StringMethod::Length => emit_length(ctx, function, llvm_function),
        StringMethod::Slice => emit_slice(ctx, function, llvm_function),
        StringMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
        StringMethod::ToCstring => emit_to_cstring(ctx, function, llvm_function),
    }
}

fn emit_byte_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function)?;
    let byte_count = load_byte_count(ctx, payload)?;
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
    let payload = self_payload(function, llvm_function)?;
    let shared = heap_payload::share_heap_payload(ctx, function.symbol.mangled(), payload)?;
    ctx.builder.build_return(Some(&shared)).or_ice().map(|_| ())
}

fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function)?;
    let helper = declare_string_length_extern(ctx);
    let value = ctx.call_basic(helper, &[payload.into()], "len")?;
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn emit_slice<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function)?;
    let range = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String.slice missing `range` param on `{}`",
            function.symbol,
        ))
    })?;
    let range_struct = match range {
        BasicValueEnum::StructValue(s) => s,
        other => {
            return Err(LlvmError::Codegen(format!(
                "String.slice expected Range struct on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };
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
    let payload = self_payload(function, llvm_function)?;
    let index = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String.get missing `index` param on `{}`",
            function.symbol,
        ))
    })?;
    let helper = declare_string_get_extern(ctx);
    let raw_ptr = ctx
        .call_basic(helper, &[payload.into(), index.into()], "ch")?
        .into_pointer_value();

    let option_symbol = expect_enum_symbol(&function.return_type, function)?;
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
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[raw_ptr.into()])?;
    ctx.builder.build_return(Some(&some)).or_ice()?;

    ctx.builder.position_at_end(none_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder.build_return(Some(&none)).or_ice().map(|_| ())
}

fn emit_to_cstring<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let payload = self_payload(function, llvm_function)?;
    let byte_len = load_byte_count(ctx, payload)?;
    let result_symbol = result::return_symbol(function)?;
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
    let cstring_type = result::single_payload_type(ctx, result_symbol, result::OK_TAG)?;
    match cstring_type {
        IRType::Struct(_) => Ok(ir_basic_type(ctx, &cstring_type)?.into_struct_type()),
        other => Err(LlvmError::Codegen(format!(
            "String.to_cstring expected a CString Ok payload, got `{other:?}`",
        ))),
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
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String intrinsic missing `self` payload pointer on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::PointerValue(p) => Ok(p),
        other => Err(LlvmError::Codegen(format!(
            "String intrinsic expected pointer receiver on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn load_byte_count<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    ctx.builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()
}

/// Extract the IR symbol of an enum type from `ty`. Mirrors the
/// helper in `intrinsics/list.rs`. The lowering pass guarantees an
/// enum-typed return for `String.get`, but the error path is kept
/// to surface IR-seal violations as codegen errors rather than
/// panics.
fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "String.get expected an enum-typed return, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ))),
    }
}

fn build_cstring<'ctx>(
    ctx: &EmitContext<'ctx>,
    cstring_ty: StructType<'ctx>,
    ptr: PointerValue<'ctx>,
    len: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let alloca = ctx.builder.build_alloca(cstring_ty, "cs_tmp").or_ice()?;
    let ptr_field = ctx
        .builder
        .build_struct_gep(cstring_ty, alloca, 0, "cs_ptr")
        .or_ice()?;
    ctx.builder.build_store(ptr_field, ptr).or_ice()?;
    let len_field = ctx
        .builder
        .build_struct_gep(cstring_ty, alloca, 1, "cs_len")
        .or_ice()?;
    ctx.builder.build_store(len_field, len).or_ice()?;
    ctx.builder
        .build_load(cstring_ty, alloca, "cs_val")
        .or_ice()
}
