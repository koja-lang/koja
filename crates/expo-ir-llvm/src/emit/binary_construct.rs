//! LLVM emission for `IRInstruction::BinaryConstruct` —
//! `<<segments>>` literal lowering.
//!
//! Two emission shapes, picked per-segment:
//!
//! - **Byte-aligned** (`bit_offset % 8 == 0 && width % 8 == 0`):
//!   pack inline. Integer / float segments run the byte-shift-loop
//!   from v1's `emit_byte_packing` (high-byte-first for `Big`,
//!   low-byte-first for `Little`); float segments first
//!   bit-cast through their integer type. String segments
//!   `memcpy` straight into the payload.
//! - **Sub-byte**: call the `__expo_alpha_pack_bits` runtime
//!   helper. Bit-shift loops on a per-byte boundary inside LLVM
//!   IR are far messier than the same logic in Rust; the helper
//!   gives us a clean Rust home for it (mirroring how `Bits`
//!   concat goes through `__expo_alpha_concat_bits`).
//!
//! The result heap block matches the v1 layout the entire
//! string/binary/bits family shares: `[i64 bit_length][payload]`,
//! with the SSA pointer pointing at the payload (`base + 8`). The
//! caller stamps `IRType::Binary` (when `layout.byte_aligned`) or
//! `IRType::Bits` on the destination value.

use expo_alpha_ir::{BinaryEndian, LoweredBinarySegment, ResolvedBinaryLayout, ValueId};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::runtime::{declare_malloc_extern, declare_pack_bits_extern};

use super::{ValueMap, inkwell_err, lookup};

/// Lower an `IRInstruction::BinaryConstruct`. Allocates a fresh
/// heap block sized to `layout.total_bits` (rounded up to bytes),
/// stamps the bit-length header, then walks `segments` in source
/// order packing each at its pre-computed `bit_offset`.
pub(super) fn emit_binary_construct<'ctx>(
    ctx: &EmitContext<'ctx>,
    layout: ResolvedBinaryLayout,
    segments: &[LoweredBinarySegment],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let total_bytes = layout.total_bits.div_ceil(8);

    let malloc = declare_malloc_extern(ctx);
    let alloc_size = i64_ty.const_int(8 + total_bytes, false);
    let base = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "bin_alloc")
        .map_err(|e| inkwell_err(format_args!("BinaryConstruct malloc"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("malloc returned void".to_string()))?
        .into_pointer_value();

    ctx.builder
        .build_store(base, i64_ty.const_int(layout.total_bits, false))
        .map_err(|e| inkwell_err(format_args!("BinaryConstruct header store"), e))?;

    let payload = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, base, &[i64_ty.const_int(8, false)], "bin_payload")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct payload GEP"), e))?
    };

    // Pre-zero the payload so trailing partial-byte bits and any
    // padding bits in sub-byte segments stay 0. The runtime
    // `pack_bits` helper `or`s into the existing byte; pre-zeroing
    // ensures unused slots are clean. (Inline byte-aligned
    // segments overwrite their bytes directly, but pre-zeroing is
    // still cheap and keeps the contract uniform.)
    if total_bytes > 0 {
        ctx.builder
            .build_memset(
                payload,
                1,
                i8_ty.const_int(0, false),
                i64_ty.const_int(total_bytes, false),
            )
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct payload memset"), e))?;
    }

    for segment in segments {
        emit_segment(ctx, payload, segment, values)?;
    }

    Ok(payload.into())
}

/// Pack a single segment into `payload` at its pre-computed
/// `bit_offset`. Dispatches on the byte-alignment fast path; sub-
/// byte segments funnel through [`pack_bits_segment`] which calls
/// the runtime helper with the segment's value coerced to `i64`.
fn emit_segment<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    segment: &LoweredBinarySegment,
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match segment {
        LoweredBinarySegment::Integer {
            value,
            width,
            endian,
            bit_offset,
            ..
        } => {
            let int_value = lookup_int_widened(ctx, values, *value)?;
            if *bit_offset % 8 == 0 && *width % 8 == 0 {
                emit_byte_packed_int(ctx, payload, int_value, *width, *endian, *bit_offset / 8)
            } else {
                pack_bits_segment(ctx, payload, int_value, *width, *bit_offset)
            }
        }
        LoweredBinarySegment::Float {
            value,
            width,
            endian,
            bit_offset,
        } => {
            let int_value = float_value_as_i64(ctx, values, *value, *width)?;
            // Floats are always byte-aligned in v1 (32 / 64 bit
            // widths), so we can lean on the byte-shift loop.
            emit_byte_packed_int(ctx, payload, int_value, *width, *endian, *bit_offset / 8)
        }
        LoweredBinarySegment::String {
            value,
            byte_length,
            bit_offset,
        } => {
            // String segments are always byte-aligned by language
            // semantics: the byte_length-derived width is a
            // multiple of 8 and the layout pre-rejects sub-byte
            // string offsets.
            let i8_ty = ctx.context.i8_type();
            let i64_ty = ctx.context.i64_type();
            let str_ptr = lookup(values, *value)?.into_pointer_value();
            let dest = unsafe {
                ctx.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        payload,
                        &[i64_ty.const_int(bit_offset / 8, false)],
                        "str_seg_dest",
                    )
                    .map_err(|e| inkwell_err(format_args!("BinaryConstruct str GEP"), e))?
            };
            ctx.builder
                .build_memcpy(dest, 1, str_ptr, 1, i64_ty.const_int(*byte_length, false))
                .map_err(|e| inkwell_err(format_args!("BinaryConstruct str memcpy"), e))?;
            Ok(())
        }
    }
}

/// Byte-by-byte pack of an integer-coerced segment value. Mirrors
/// v1's `emit_byte_packing` — for `num_bytes = width / 8`, write
/// each output byte from MSB-first (Big) or LSB-first (Little) by
/// shifting the i64 right and truncating to `i8`.
fn emit_byte_packed_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    val_i64: IntValue<'ctx>,
    width: u64,
    endian: BinaryEndian,
    byte_offset: u64,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let num_bytes = width / 8;
    for i in 0..num_bytes {
        let shift_amount = match endian {
            BinaryEndian::Little => i * 8,
            BinaryEndian::Big => (num_bytes - 1 - i) * 8,
        };
        let shifted = if shift_amount > 0 {
            ctx.builder
                .build_right_shift(
                    val_i64,
                    i64_ty.const_int(shift_amount, false),
                    false,
                    "seg_shr",
                )
                .map_err(|e| inkwell_err(format_args!("BinaryConstruct byte shr"), e))?
        } else {
            val_i64
        };
        let byte = ctx
            .builder
            .build_int_truncate(shifted, i8_ty, "seg_byte")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct byte trunc"), e))?;
        let dest = unsafe {
            ctx.builder
                .build_in_bounds_gep(
                    i8_ty,
                    payload,
                    &[i64_ty.const_int(byte_offset + i, false)],
                    "seg_byte_ptr",
                )
                .map_err(|e| inkwell_err(format_args!("BinaryConstruct byte GEP"), e))?
        };
        ctx.builder
            .build_store(dest, byte)
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct byte store"), e))?;
    }
    Ok(())
}

/// Sub-byte segment: hand the i64-widened value, the bit `width`,
/// and the absolute `bit_offset` to the runtime helper. Endianness
/// is meaningless for non-byte-multiple widths in v1, so we only
/// emit MSB-first; the helper writes the low `width` bits of
/// `value` left-to-right starting at `bit_offset`.
fn pack_bits_segment<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    val_i64: IntValue<'ctx>,
    width: u64,
    bit_offset: u64,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let helper = declare_pack_bits_extern(ctx);
    let width_arg = i8_ty.const_int(width, false);
    let offset_arg = i64_ty.const_int(bit_offset, false);
    ctx.builder
        .build_call(
            helper,
            &[
                payload.into(),
                val_i64.into(),
                width_arg.into(),
                offset_arg.into(),
            ],
            "pack_bits_call",
        )
        .map_err(|e| inkwell_err(format_args!("__expo_alpha_pack_bits call"), e))?;
    Ok(())
}

/// Look up an integer-typed segment value and widen it to `i64`
/// (or truncate, for the unlikely > 64-bit case). The runtime
/// `__expo_alpha_pack_bits` helper, the byte-pack loop, and the
/// float packer all operate on `i64` so segment values converge
/// at this seam.
fn lookup_int_widened<'ctx>(
    ctx: &EmitContext<'ctx>,
    values: &ValueMap<'ctx>,
    id: ValueId,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = lookup(values, id)?.into_int_value();
    let bits = raw.get_type().get_bit_width();
    let i64_ty = ctx.context.i64_type();
    let widened = match bits.cmp(&64) {
        std::cmp::Ordering::Less => ctx
            .builder
            .build_int_z_extend(raw, i64_ty, "seg_widen")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct seg widen"), e))?,
        std::cmp::Ordering::Greater => ctx
            .builder
            .build_int_truncate(raw, i64_ty, "seg_trunc")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct seg trunc"), e))?,
        std::cmp::Ordering::Equal => raw,
    };
    // Use the predicate constant so the unused-import lint stays
    // honest if we drop the `widen` path.
    let _ = IntPredicate::EQ;
    Ok(widened)
}

/// Bit-cast a float segment's value to its `i{width}` representation
/// and zero-extend to i64. v1 semantics: `Float32` segments
/// truncate from the IR's i64 / Float64 first.
fn float_value_as_i64<'ctx>(
    ctx: &EmitContext<'ctx>,
    values: &ValueMap<'ctx>,
    id: ValueId,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = lookup(values, id)?;
    let i64_ty = ctx.context.i64_type();
    if width == 32 {
        let f32_val = if raw.is_float_value() {
            let fv = raw.into_float_value();
            if fv.get_type() == ctx.context.f32_type() {
                fv
            } else {
                ctx.builder
                    .build_float_trunc(fv, ctx.context.f32_type(), "f32_trunc")
                    .map_err(|e| inkwell_err(format_args!("BinaryConstruct f32 trunc"), e))?
            }
        } else {
            return Err(LlvmError::Codegen(
                "BinaryConstruct: Float32 segment received non-float value".to_string(),
            ));
        };
        let i32_bits = ctx
            .builder
            .build_bit_cast(f32_val, ctx.context.i32_type(), "f32_bits")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct f32 bitcast"), e))?
            .into_int_value();
        let widened = ctx
            .builder
            .build_int_z_extend(i32_bits, i64_ty, "f32_to_i64")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct f32 zext"), e))?;
        return Ok(widened);
    }
    if width == 64 {
        let f64_val = if raw.is_float_value() {
            let fv = raw.into_float_value();
            if fv.get_type() == ctx.context.f64_type() {
                fv
            } else {
                ctx.builder
                    .build_float_ext(fv, ctx.context.f64_type(), "f64_ext")
                    .map_err(|e| inkwell_err(format_args!("BinaryConstruct f64 ext"), e))?
            }
        } else {
            return Err(LlvmError::Codegen(
                "BinaryConstruct: Float64 segment received non-float value".to_string(),
            ));
        };
        let i64_bits = ctx
            .builder
            .build_bit_cast(f64_val, i64_ty, "f64_bits")
            .map_err(|e| inkwell_err(format_args!("BinaryConstruct f64 bitcast"), e))?
            .into_int_value();
        return Ok(i64_bits);
    }
    Err(LlvmError::Codegen(format!(
        "BinaryConstruct: unsupported float width {width} (expected 32 or 64) — \
         seal invariant violation",
    )))
}
