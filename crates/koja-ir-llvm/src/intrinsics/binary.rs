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
use crate::emit::heap_layout::load_bit_length;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::heap_payload;
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
        BinaryMethod::Ptr => emit_self_passthrough(ctx, function, llvm_function),
        BinaryMethod::ToBits => emit_to_bits(ctx, function, llvm_function),
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
        BitsMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
    }
}

/// `Binary.to_bits(self) -> Bits` — a zero-cost reinterpret: `Binary`
/// and `Bits` share the identical `[rc][bit_length][bytes]` block, so
/// we rc-acquire the immutable block and hand back the same payload
/// pointer as an owned `Bits`. The matching `Drop` rc-decrements.
fn emit_to_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let shared = heap_payload::share_heap_payload(ctx, function.symbol.mangled(), payload)?;
    ctx.builder.build_return(Some(&shared)).or_ice().map(|_| ())
}

/// `byte_size = bit_length / 8`. Reads the i64 header at
/// `payload_ptr - 8`, shifts right by 3 (logical), returns Int.
fn emit_byte_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()?;
    ctx.builder
        .build_return(Some(&byte_count))
        .or_ice()
        .map(|_| ())
}

/// Zero-cost conversion. Both `Binary` and `Bits` lower to the same
/// payload-pointer shape (`ptr` at the LLVM layer); `Binary.ptr`
/// hands back a `CPtr<UInt8>` shaped identically. Just return `self`.
fn emit_self_passthrough<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    ctx.builder
        .build_return(Some(&payload))
        .or_ice()
        .map(|_| ())
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
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let i64_ty = ctx.context.i64_type();
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()?;

    let validate = declare_utf8_validate_extern(ctx);
    let is_valid = ctx
        .call_basic(validate, &[payload.into(), byte_count.into()], "utf8_ok")?
        .into_int_value();
    let succeeded = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            is_valid,
            i64_ty.const_int(0, false),
            "succeeded",
        )
        .or_ice()?;

    let valid_bb = ctx.context.append_basic_block(llvm_function, "valid");
    let invalid_bb = ctx.context.append_basic_block(llvm_function, "invalid");
    ctx.builder
        .build_conditional_branch(succeeded, valid_bb, invalid_bb)
        .or_ice()?;

    ctx.builder.position_at_end(valid_bb);
    let new_payload =
        heap_payload::copy_heap_payload(ctx, function.symbol.mangled(), payload, true, false)?;
    return_result(ctx, result_symbol, RESULT_OK_TAG, new_payload.into())?;

    emit_err_branch(ctx, invalid_bb, result_symbol, b"invalid UTF-8")
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
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let i64_ty = ctx.context.i64_type();
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    let remainder = ctx
        .builder
        .build_and(bit_length, i64_ty.const_int(7, false), "remainder")
        .or_ice()?;
    let is_aligned = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            remainder,
            i64_ty.const_int(0, false),
            "is_aligned",
        )
        .or_ice()?;

    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(is_aligned, ok_bb, err_bb)
        .or_ice()?;

    // Zero-cost reinterpret: `Bits` and `Binary` share the identical
    // `[rc][bit_length][bytes]` block (this branch only runs when
    // `bit_length & 7 == 0`, so the payload is exactly byte-aligned),
    // so rc-acquire the immutable block and hand back its pointer.
    ctx.builder.position_at_end(ok_bb);
    let shared = heap_payload::share_heap_payload(ctx, function.symbol.mangled(), payload)?;
    return_result(ctx, result_symbol, RESULT_OK_TAG, shared.into())?;

    emit_err_branch(
        ctx,
        err_bb,
        result_symbol,
        b"bit length is not byte-aligned",
    )
}

fn emit_err_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: BasicBlock<'ctx>,
    result_symbol: &IRSymbol,
    message: &[u8],
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let err_msg = emit_string_literal_payload(ctx, message, "binary_err");
    return_result(ctx, result_symbol, RESULT_ERR_TAG, err_msg.into())
}

fn return_result<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    tag: IRVariantTag,
    payload: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let value = build_enum_value(ctx, result_symbol, tag, &[payload])?;
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
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
