//! `Binary.*` and `Bits.*` intrinsic family. Both layouts share the
//! `[i64 bit_length][ceil(bit_length / 8) bytes]` heap shape; the
//! returned pointer points at the payload, with the bit-length
//! header at offset `-8`. Conversions between `Binary` and `Bits`
//! are zero-cost when the lengths line up.
//!
//! - `Binary.byte_size(self) -> Int` — divides the header by 8.
//! - `Binary.ptr(self) -> CPtr<UInt8>` — returns `self` (the payload
//!   pointer is already byte-addressable).
//! - `Binary.to_bits(self) -> Bits` — zero-cost reinterpret.
//! - `Binary.to_string(self) -> Result<String, String>` — validates
//!   UTF-8 via the `koja_utf8_validate` runtime helper, then
//!   heap-copies into a NUL-terminated `String` payload on success.
//!   Error branch returns `Result.Err("invalid UTF-8")`. Mirrors
//!   v1's `Binary_to_string` codegen one-for-one (same runtime
//!   helper, same Ok/Err shapes).
//! - `Bits.to_binary(self) -> Result<Binary, String>` — checks
//!   `bit_length & 7 == 0` and returns `Result.Ok(self)` (zero-cost
//!   reinterpret, both layouts are identical) when aligned, or
//!   `Result.Err("bit length is not byte-aligned")` otherwise.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};
use koja_ir::{BinaryMethod, BitsMethod, IRFunction, IRSymbol, IRType, IRVariantTag};

use crate::ctx::EmitContext;
use crate::emit::constants::emit_string_literal_payload;
use crate::emit::enums::build_enum_value;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::heap_clone::{self, HEADER_BYTES};
use crate::runtime::declare_utf8_validate_extern;

/// `enum Result<T, E>` variant tag for `Ok(T)` — declaration order
/// in `koja/lib/global/src/kernel.koja`.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
/// `enum Result<T, E>` variant tag for `Err(E)`.
const RESULT_ERR_TAG: IRVariantTag = IRVariantTag(1);

pub(super) fn emit_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: BinaryMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match method {
        BinaryMethod::ByteSize => emit_byte_size(ctx, function, llvm_function),
        BinaryMethod::Clone => {
            heap_clone::emit_payload_clone(ctx, function, llvm_function, false, false)
        }
        BinaryMethod::Ptr | BinaryMethod::ToBits => {
            emit_self_passthrough(ctx, function, llvm_function)
        }
        BinaryMethod::ToString => emit_to_string(ctx, function, llvm_function),
    }
}

pub(super) fn emit_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: BitsMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match method {
        BitsMethod::Clone => {
            heap_clone::emit_payload_clone(ctx, function, llvm_function, false, true)
        }
        BitsMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
    }
}

/// `byte_size = bit_length / 8`. Reads the i64 header at
/// `payload_ptr - 8`, shifts right by 3 (logical), returns Int.
fn emit_byte_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let payload = heap_clone::pointer_param(function, llvm_function)?;
    let neg = i64_ty.const_int((-(HEADER_BYTES as i64)) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg], "hdr_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let bit_length = ctx
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_right_shift for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(Some(&byte_count))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// Zero-cost conversion. Both `Binary` and `Bits` lower to the same
/// payload-pointer shape (`ptr` at the LLVM layer); `Binary.ptr`
/// hands back a `CPtr<UInt8>` shaped identically. Just return `self`.
fn emit_self_passthrough<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = heap_clone::pointer_param(function, llvm_function)?;
    ctx.builder
        .build_return(Some(&payload))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// `Binary.to_string`: validate UTF-8 via the runtime helper, then
/// heap-copy the payload into a fresh NUL-terminated `String`
/// allocation and return `Result.Ok(payload)`. Invalid UTF-8 falls
/// through to `Result.Err("invalid UTF-8")`.
fn emit_to_string<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function)?;
    let payload = heap_clone::pointer_param(function, llvm_function)?;
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let neg_hdr = i64_ty.const_int((-(HEADER_BYTES as i64)) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg_hdr], "hdr_ptr")
            .map_err(|e| {
                inkwell_err(
                    format_args!("to_string hdr GEP for `{}`", function.symbol),
                    e,
                )
            })?
    };
    let bit_length = ctx
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .map_err(|e| {
            inkwell_err(
                format_args!("to_string hdr load for `{}`", function.symbol),
                e,
            )
        })?
        .into_int_value();
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .map_err(|e| {
            inkwell_err(
                format_args!("to_string byte_count for `{}`", function.symbol),
                e,
            )
        })?;

    let validate = declare_utf8_validate_extern(ctx);
    let is_valid = ctx
        .builder
        .build_call(validate, &[payload.into(), byte_count.into()], "utf8_ok")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call koja_utf8_validate for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "koja_utf8_validate returned no value for `{}`",
                function.symbol,
            ))
        })?
        .into_int_value();
    let succeeded = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            is_valid,
            i64_ty.const_int(0, false),
            "succeeded",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;

    let valid_bb = ctx.context.append_basic_block(llvm_function, "valid");
    let invalid_bb = ctx.context.append_basic_block(llvm_function, "invalid");
    ctx.builder
        .build_conditional_branch(succeeded, valid_bb, invalid_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(valid_bb);
    let new_payload = heap_clone::copy_heap_payload(ctx, function, payload, true, false)?;
    return_result(
        ctx,
        function,
        result_symbol,
        RESULT_OK_TAG,
        new_payload.into(),
    )?;

    emit_err_branch(ctx, function, invalid_bb, result_symbol, b"invalid UTF-8")
}

/// `Bits.to_binary`: branch on `bit_length & 7 == 0`. The aligned
/// branch returns `Result.Ok(self)` (Bits and Binary share the
/// payload-pointer + header layout). The unaligned branch returns
/// `Result.Err("bit length is not byte-aligned")`.
fn emit_to_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let result_symbol = expect_enum_symbol(&function.return_type, function)?;
    let payload = heap_clone::pointer_param(function, llvm_function)?;
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let neg_hdr = i64_ty.const_int((-(HEADER_BYTES as i64)) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg_hdr], "hdr_ptr")
            .map_err(|e| {
                inkwell_err(
                    format_args!("to_binary hdr GEP for `{}`", function.symbol),
                    e,
                )
            })?
    };
    let bit_length = ctx
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .map_err(|e| {
            inkwell_err(
                format_args!("to_binary hdr load for `{}`", function.symbol),
                e,
            )
        })?
        .into_int_value();
    let remainder = ctx
        .builder
        .build_and(bit_length, i64_ty.const_int(7, false), "remainder")
        .map_err(|e| inkwell_err(format_args!("to_binary `&7` for `{}`", function.symbol), e))?;
    let is_aligned = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            remainder,
            i64_ty.const_int(0, false),
            "is_aligned",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("to_binary build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;

    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(is_aligned, ok_bb, err_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!(
                    "to_binary build_conditional_branch for `{}`",
                    function.symbol
                ),
                e,
            )
        })?;

    ctx.builder.position_at_end(ok_bb);
    return_result(ctx, function, result_symbol, RESULT_OK_TAG, payload.into())?;

    emit_err_branch(
        ctx,
        function,
        err_bb,
        result_symbol,
        b"bit length is not byte-aligned",
    )
}

fn emit_err_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    block: BasicBlock<'ctx>,
    result_symbol: &IRSymbol,
    message: &[u8],
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let err_msg = emit_string_literal_payload(ctx, message, "binary_err");
    return_result(ctx, function, result_symbol, RESULT_ERR_TAG, err_msg.into())
}

fn return_result<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    result_symbol: &IRSymbol,
    tag: IRVariantTag,
    payload: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let value = build_enum_value(ctx, result_symbol, tag, &[payload])?;
    ctx.builder
        .build_return(Some(&value))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "binary intrinsic on `{}` expected an enum-typed return, got `{other:?}`",
            function.symbol,
        ))),
    }
}
