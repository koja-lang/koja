//! `Binary.*` and `Bits.*` intrinsic family. Both layouts share the
//! `[i64 bit_length][ceil(bit_length / 8) bytes]` heap shape. The
//! returned pointer points at the payload, with the bit-length
//! header at offset `-8`. Conversions between `Binary` and `Bits`
//! are zero-cost when the lengths line up.
//!
//! - `Binary.at(self, index: Int) -> Option<Int>`: O(1) byte read.
//!   Bounds-check against the header, then GEP + load + zext. No
//!   runtime call.
//! - `Binary.byte_size(self) -> Int`: divides the header by 8.
//! - `Binary.slice(self, range: Range) -> Binary`: copies the
//!   inclusive byte range `[start, stop]` via the
//!   `koja_binary_slice` runtime helper. Endpoints clamp.
//! - `Binary.to_bits(self) -> Bits`: zero-cost reinterpret.
//! - `Binary.to_string(self) -> Result<String, String.ConversionError>`: validates
//!   UTF-8 via the `koja_utf8_validate` runtime helper, then
//!   heap-copies into a NUL-terminated `String` payload on success.
//!   The error branch returns `String.ConversionError.InvalidUTF8`.
//! - `Bits.bit_size(self) -> Int`: returns the header verbatim.
//! - `Bits.byte_at(self, index: Int) -> Option<Int>`: like
//!   `Binary.at`, but bounds span `ceil(bit_length / 8)` bytes so
//!   the trailing partial byte stays addressable.
//! - `Bits.to_binary(self) -> Result<Binary, String>`: checks
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
use crate::intrinsics::result;
use crate::runtime::{declare_binary_slice_extern, declare_utf8_validate_extern};

/// `enum Result<T, E>` variant tag for `Ok(T)`: declaration order
/// in `koja/lib/global/src/kernel.koja`.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
/// `enum Result<T, E>` variant tag for `Err(E)`.
const RESULT_ERR_TAG: IRVariantTag = IRVariantTag(1);
/// `enum Option<T>` variant tags: declaration order in
/// `koja/lib/global/src/kernel.koja`.
const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

pub(super) fn emit_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: BinaryMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match method {
        BinaryMethod::At => emit_at(ctx, function, llvm_function),
        BinaryMethod::ByteSize => emit_byte_size(ctx, function, llvm_function),
        BinaryMethod::Slice => emit_slice(ctx, function, llvm_function),
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
        BitsMethod::BitSize => emit_bit_size(ctx, function, llvm_function),
        BitsMethod::ByteAt => emit_bits_byte_at(ctx, function, llvm_function),
        BitsMethod::ToBinary => emit_to_binary(ctx, function, llvm_function),
    }
}

/// `Binary.to_bits(self) -> Bits` is a zero-cost reinterpret: `Binary`
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

/// `Binary.at(self, index) -> Option<Int>` is pure inline IR: load
/// the byte count from the header, bounds-check the index, then
/// GEP + load + zext the byte. Out-of-bounds returns `Option.None`.
fn emit_at<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    emit_byte_lookup(ctx, function, llvm_function, false)
}

/// `Bits.byte_at(self, index) -> Option<Int>`: same shape as
/// [`emit_at`], but the payload spans `ceil(bit_length / 8)` bytes
/// so a trailing partial byte stays addressable.
fn emit_bits_byte_at<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    emit_byte_lookup(ctx, function, llvm_function, true)
}

/// Shared body for the indexed byte reads. `ceil_bytes` selects the
/// bounds arithmetic: floor for `Binary` (always byte-aligned), ceil
/// for `Bits` (the last byte may be partial).
fn emit_byte_lookup<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    ceil_bytes: bool,
) -> Result<(), LlvmError> {
    let option_symbol = expect_enum_symbol(&function.return_type, function)?;
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let index = llvm_function
        .get_nth_param(1)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "byte lookup missing `index` param on `{}`",
                function.symbol,
            ))
        })?
        .into_int_value();

    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    let shift_input = if ceil_bytes {
        ctx.builder
            .build_int_add(bit_length, i64_ty.const_int(7, false), "bits_rounded")
            .or_ice()?
    } else {
        bit_length
    };
    let byte_count = ctx
        .builder
        .build_right_shift(shift_input, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()?;
    let nonnegative = ctx
        .builder
        .build_int_compare(IntPredicate::SGE, index, i64_ty.const_zero(), "nonnegative")
        .or_ice()?;
    let below_count = ctx
        .builder
        .build_int_compare(IntPredicate::SLT, index, byte_count, "below_count")
        .or_ice()?;
    let in_bounds = ctx
        .builder
        .build_and(nonnegative, below_count, "in_bounds")
        .or_ice()?;

    let some_bb = ctx.context.append_basic_block(llvm_function, "some");
    let none_bb = ctx.context.append_basic_block(llvm_function, "none");
    ctx.builder
        .build_conditional_branch(in_bounds, some_bb, none_bb)
        .or_ice()?;

    ctx.builder.position_at_end(some_bb);
    let byte_ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, payload, &[index], "byte_ptr")
            .or_ice()?
    };
    let byte = ctx
        .builder
        .build_load(i8_ty, byte_ptr, "byte")
        .or_ice()?
        .into_int_value();
    let widened = ctx
        .builder
        .build_int_z_extend(byte, i64_ty, "widened")
        .or_ice()?;
    let some = build_enum_value(ctx, option_symbol, OPTION_SOME_TAG, &[widened.into()])?;
    ctx.builder.build_return(Some(&some)).or_ice()?;

    ctx.builder.position_at_end(none_bb);
    let none = build_enum_value(ctx, option_symbol, OPTION_NONE_TAG, &[])?;
    ctx.builder.build_return(Some(&none)).or_ice().map(|_| ())
}

/// `Binary.slice(self, range) -> Binary`: unpack the `Range`
/// struct's `start` / `stop` fields and delegate the clamped copy to
/// the `koja_binary_slice` runtime helper.
fn emit_slice<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let range = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Binary.slice missing `range` param on `{}`",
            function.symbol,
        ))
    })?;
    let BasicValueEnum::StructValue(range_struct) = range else {
        return Err(LlvmError::Codegen(format!(
            "Binary.slice expected Range struct on `{}`, got `{range:?}`",
            function.symbol,
        )));
    };
    let start = ctx
        .builder
        .build_extract_value(range_struct, 0, "start")
        .or_ice()?;
    let stop = ctx
        .builder
        .build_extract_value(range_struct, 1, "stop")
        .or_ice()?;
    let helper = declare_binary_slice_extern(ctx);
    let sliced = ctx.call_basic(
        helper,
        &[payload.into(), start.into(), stop.into()],
        "sliced",
    )?;
    ctx.builder.build_return(Some(&sliced)).or_ice().map(|_| ())
}

/// `Bits.bit_size` returns the i64 header at `payload_ptr - 8`
/// verbatim, with no byte conversion.
fn emit_bit_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = heap_payload::pointer_param(function, llvm_function)?;
    let bit_length = load_bit_length(ctx, payload, "bit_length")?;
    ctx.builder
        .build_return(Some(&bit_length))
        .or_ice()
        .map(|_| ())
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

/// `Binary.to_string`: validate UTF-8 via the runtime helper, then
/// heap-copy the payload into a fresh NUL-terminated `String`
/// allocation and return `Result.Ok(payload)`. Invalid UTF-8 returns
/// `String.ConversionError.InvalidUTF8`.
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
    let ok = result::build_ok(ctx, result_symbol, new_payload.into())?;
    ctx.builder.build_return(Some(&ok)).or_ice()?;

    ctx.builder.position_at_end(invalid_bb);
    let error = result::build_unit_error(ctx, result_symbol, "InvalidUTF8")?;
    ctx.builder.build_return(Some(&error)).or_ice().map(|_| ())
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
