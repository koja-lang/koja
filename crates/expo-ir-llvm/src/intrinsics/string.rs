//! `String` method intrinsics. Mirrors v1's
//! `expo-codegen::intrinsics::string` + `cptr::emit_cstring_intrinsic`
//! port shape: trivial cells inline against the `[i64 bit_length]
//! [payload bytes]` layout (with the SSA pointer pointing at the
//! payload), and the codepoint-aware cells (`length`, `get`,
//! `slice`) delegate to `expo-runtime` helpers so unicode walking
//! stays in Rust.

use expo_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, StringMethod};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::intrinsics::heap_clone;
use crate::runtime::{
    declare_malloc_extern, declare_string_get_extern, declare_string_length_extern,
    declare_string_slice_extern,
};
use crate::types::ir_basic_type;

/// `Option<T>` variant tags, matching the stdlib decl order: `Some`
/// first (tag 0), then `None`.
const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

/// `[i64 bit_length][payload bytes]` — the SSA pointer points at
/// the first payload byte; the bit-length sits 8 bytes before.
const STRING_HEADER_BYTES: u64 = 8;

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
        StringMethod::Clone => {
            heap_clone::emit_payload_clone(ctx, function, llvm_function, true, false)
        }
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
    let byte_count = load_byte_count(ctx, function, payload)?;
    ctx.builder
        .build_return(Some(&byte_count))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_to_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function)?;
    ctx.builder
        .build_return(Some(&payload))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = self_payload(function, llvm_function)?;
    let helper = declare_string_length_extern(ctx);
    let call = ctx
        .builder
        .build_call(helper, &[payload.into()], "len")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call expo_string_length for `{}`", function.symbol),
                e,
            )
        })?;
    let value = call.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "expo_string_length returned no value for `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
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
        .map_err(|e| {
            inkwell_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?;
    let stop = ctx
        .builder
        .build_extract_value(range_struct, 1, "stop")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?;
    let helper = declare_string_slice_extern(ctx);
    let call = ctx
        .builder
        .build_call(
            helper,
            &[payload.into(), start.into(), stop.into()],
            "sliced",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call expo_string_slice for `{}`", function.symbol),
                e,
            )
        })?;
    let value = call.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "expo_string_slice returned no value for `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
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
    let call = ctx
        .builder
        .build_call(helper, &[payload.into(), index.into()], "ch")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call expo_string_get for `{}`", function.symbol),
                e,
            )
        })?;
    let raw_ptr = call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "expo_string_get returned no value for `{}`",
                function.symbol,
            ))
        })?
        .into_pointer_value();

    let option_symbol = expect_enum_symbol(&function.return_type, function)?;
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let is_null = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, raw_ptr, ptr_ty.const_null(), "is_null")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;

    let some_bb = ctx.context.append_basic_block(llvm_function, "some");
    let none_bb = ctx.context.append_basic_block(llvm_function, "none");
    ctx.builder
        .build_conditional_branch(is_null, none_bb, some_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(some_bb);
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[raw_ptr.into()])?;
    ctx.builder
        .build_return(Some(&some))
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(none_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder
        .build_return(Some(&none))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_to_cstring<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let payload = self_payload(function, llvm_function)?;
    let byte_len = load_byte_count(ctx, function, payload)?;

    let alloc_size = ctx
        .builder
        .build_int_add(byte_len, i64_ty.const_int(1, false), "alloc_size")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let malloc = declare_malloc_extern(ctx);
    let buf = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "c_buf")
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
                function.symbol,
            ))
        })?
        .into_pointer_value();

    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(memcpy, &[buf.into(), payload.into(), byte_len.into()], "")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call memcpy for `{}`", function.symbol),
                e,
            )
        })?;
    let nul_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, buf, &[byte_len], "nul")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    ctx.builder
        .build_store(nul_ptr, i8_ty.const_zero())
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;

    let cstring_ty = ir_basic_type(ctx, &function.return_type)?.into_struct_type();
    let cstring = build_cstring(ctx, function, cstring_ty, buf, byte_len)?;
    ctx.builder
        .build_return(Some(&cstring))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
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
    function: &IRFunction,
    payload: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg_hdr], "hdr_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let bit_length = ctx
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    ctx.builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_right_shift for `{}`", function.symbol),
                e,
            )
        })
}

/// Extract the IR symbol of an enum type from `ty`. Mirrors the
/// helper in `intrinsics/list.rs` — the lowering pass guarantees an
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
    function: &IRFunction,
    cstring_ty: StructType<'ctx>,
    ptr: PointerValue<'ctx>,
    len: IntValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let alloca = ctx
        .builder
        .build_alloca(cstring_ty, "cs_tmp")
        .map_err(|e| inkwell_err(format_args!("build_alloca for `{}`", function.symbol), e))?;
    let ptr_field = ctx
        .builder
        .build_struct_gep(cstring_ty, alloca, 0, "cs_ptr")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_store(ptr_field, ptr)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let len_field = ctx
        .builder
        .build_struct_gep(cstring_ty, alloca, 1, "cs_len")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_store(len_field, len)
        .map_err(|e| inkwell_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_load(cstring_ty, alloca, "cs_val")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))
}
