//! LLVM emission for `IRInstruction::BinaryMatch`, the `<<segments>>`
//! binary pattern matching instruction.
//!
//! Algorithm:
//!
//! 1. Load the subject's runtime bit length from `subject - 8`,
//!    shift right by 3 for the byte length.
//! 2. Compare the byte length against `layout.fixed_bits >> 3`
//!    (`EQ` when there's no greedy tail, `UGE` when there is).
//!    The result is the seed `i1`.
//! 3. Walk segments in source order, ANDing each segment's
//!    per-segment success bit into the running result:
//!    - [`LoweredBinaryPattern::LiteralInt`] / `LiteralBytes`
//!      compare against the constant.
//!    - [`LoweredBinaryPattern::BindInt`] extracts the bit slice,
//!      sign-extends when the modifier asks for it (**fixes a v1
//!      bug**: v1 always treated the extracted value as
//!      unsigned), narrows to the slot's declared LLVM type, and
//!      stores it via the local slot table.
//!    - [`LoweredBinaryPattern::Discard`] is a no-op: only the
//!      `bit_offset` accumulator advances (the IR layer already
//!      tracked that).
//!    - [`LoweredBinaryPattern::GreedyTail`] allocates a fresh
//!      heap block of `8 + ceil(remaining_bits / 8)` bytes,
//!      copies the remaining bytes from the subject, stores the
//!      bit-length header, and writes the payload pointer into
//!      the binding slot (when there is one).
//!
//! All sub-byte arithmetic is gated to the byte-aligned path.
//! Typecheck rejects bit-misaligned greedy tails, but a `Bits`
//! greedy tail with a byte-aligned fixed prefix and a sub-byte
//! suffix still flows through here: we memcpy
//! `ceil(remaining_bits / 8)` bytes and let the heap layout carry
//! the exact bit count.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use koja_ir::{
    BinaryEndian, BinarySign, IRLocalId, IRType, LoweredBinaryMatchLayout, LoweredBinaryPattern,
    ValueId,
};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::{declare_memcmp_extern, declare_memcpy_extern};
use crate::runtime::declare_malloc_extern;

use super::constants::emit_string_literal_payload;
use super::heap_layout::{block_alloc_size, init_heap_block};
use super::{ValueMap, lookup};

/// Lower an `IRInstruction::BinaryMatch`. Returns the `i1` success
/// bit. Binding segments stamp their extracted values into the
/// pre-declared local slots as a side effect.
pub(super) fn emit_binary_match<'ctx>(
    ctx: &EmitContext<'ctx>,
    layout: LoweredBinaryMatchLayout,
    segments: &[LoweredBinaryPattern],
    subject: ValueId,
    values: &ValueMap<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let payload = lookup(values, subject)?.into_pointer_value();
    let bit_length = load_subject_bit_length(ctx, payload)?;
    let byte_length = shift_right_by_three(ctx, bit_length)?;
    let length_ok = length_check(ctx, &layout, byte_length)?;

    // Segment extraction indexes off the subject length, so on a
    // too-short subject the reads run past the payload and the
    // greedy-tail size underflows to a huge `malloc` -> null -> SIGBUS.
    // Gate it behind the length check: a failed check short-circuits to
    // `false` without touching the payload.
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen("binary match emitted with no active block".to_string())
    })?;
    let function = entry_block.get_parent().ok_or_else(|| {
        LlvmError::Codegen("binary match active block has no parent function".to_string())
    })?;
    let extract_block = ctx.context.append_basic_block(function, "bin_pat_extract");
    let merge_block = ctx.context.append_basic_block(function, "bin_pat_merge");
    ctx.builder
        .build_conditional_branch(length_ok, extract_block, merge_block)
        .or_ice()?;

    ctx.builder.position_at_end(extract_block);
    let mut extracted = true_i1(ctx);
    for segment in segments {
        let segment_ok = emit_segment(ctx, payload, bit_length, byte_length, segment)?;
        extracted = ctx
            .builder
            .build_and(extracted, segment_ok, "bin_pat_and")
            .or_ice()?;
    }
    // `emit_segment` is straight-line, but capture the builder's block
    // for the phi edge in case that ever stops being true.
    let extract_end = ctx.builder.get_insert_block().unwrap_or(extract_block);
    ctx.builder
        .build_unconditional_branch(merge_block)
        .or_ice()?;

    ctx.builder.position_at_end(merge_block);
    let result = ctx
        .builder
        .build_phi(ctx.context.bool_type(), "bin_pat_result")
        .or_ice()?;
    let length_failed = ctx.context.bool_type().const_int(0, false);
    result.add_incoming(&[(&length_failed, entry_block), (&extracted, extract_end)]);
    Ok(result.as_basic_value().into_int_value())
}

/// Read `i64 bit_length` from `payload - 8`. The IR contract puts
/// the SSA pointer at the payload, with the length header eight
/// bytes earlier.
fn load_subject_bit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let header = unsafe {
        ctx.builder
            .build_gep(
                i8_ty,
                payload,
                &[i64_ty.const_int((-8i64) as u64, true)],
                "bin_pat_len_ptr",
            )
            .or_ice()?
    };
    let loaded = ctx
        .builder
        .build_load(i64_ty, header, "bin_pat_bit_len")
        .or_ice()?;
    Ok(loaded.into_int_value())
}

/// `byte_length = bit_length >> 3`. Logical right shift since the
/// IR-side contract is that `bit_length` fits in non-negative
/// `i64` (a `usize`-sized number of bits).
fn shift_right_by_three<'ctx>(
    ctx: &EmitContext<'ctx>,
    bit_length: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    ctx.builder
        .build_right_shift(
            bit_length,
            i64_ty.const_int(3, false),
            false,
            "bin_pat_byte_len",
        )
        .or_ice()
}

/// `byte_length == fixed_bits / 8` (exact match) when the pattern
/// has no greedy tail, `byte_length >= fixed_bits / 8` (prefix
/// match) when it does. `fixed_bits` is always a multiple of 8 at
/// this point. The only sub-byte sources are literal segments
/// with sub-byte widths, which lower-side rejects.
fn length_check<'ctx>(
    ctx: &EmitContext<'ctx>,
    layout: &LoweredBinaryMatchLayout,
    byte_length: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let required_bytes = layout.fixed_bits / 8;
    let pred = if layout.has_greedy_tail {
        IntPredicate::UGE
    } else {
        IntPredicate::EQ
    };
    ctx.builder
        .build_int_compare(
            pred,
            byte_length,
            i64_ty.const_int(required_bytes, false),
            "bin_pat_len_ok",
        )
        .or_ice()
}

/// Dispatch on the per-segment variant. Returns an `i1` that the
/// caller ANDs into the running success bit.
fn emit_segment<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_length: IntValue<'ctx>,
    byte_length: IntValue<'ctx>,
    segment: &LoweredBinaryPattern,
) -> Result<IntValue<'ctx>, LlvmError> {
    match segment {
        LoweredBinaryPattern::LiteralInt {
            bit_offset,
            endian,
            sign,
            value,
            width,
        } => emit_literal_int(ctx, payload, *bit_offset, *endian, *sign, *value, *width),
        LoweredBinaryPattern::LiteralBytes { bit_offset, bytes } => {
            emit_literal_bytes(ctx, payload, *bit_offset, bytes)
        }
        LoweredBinaryPattern::BindInt {
            bit_offset,
            endian,
            local,
            sign,
            ty,
            width,
        } => {
            emit_bind_int(
                ctx,
                payload,
                *bit_offset,
                *endian,
                *local,
                *sign,
                ty,
                *width,
            )?;
            Ok(true_i1(ctx))
        }
        LoweredBinaryPattern::Discard { .. } => Ok(true_i1(ctx)),
        LoweredBinaryPattern::GreedyTail {
            bit_offset,
            local,
            ty,
        } => {
            emit_greedy_tail(
                ctx,
                payload,
                bit_length,
                byte_length,
                *bit_offset,
                *local,
                ty,
            )?;
            Ok(true_i1(ctx))
        }
    }
}

/// Compare the byte-aligned slice at `bit_offset` against the
/// constant `value`. Sub-byte widths flow through here too, but
/// only at sub-byte `bit_offset`s. For now we gate to byte
/// alignment. The literal-only path that hits sub-byte widths
/// is `<<x::3, _::5>>`-style and isn't required by any current
/// test.
fn emit_literal_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_offset: u64,
    endian: BinaryEndian,
    _sign: BinarySign,
    value: i128,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    if !bit_offset.is_multiple_of(8) || !width.is_multiple_of(8) {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: sub-byte binary literal pattern segment (bit_offset={bit_offset}, \
             width={width}) is not yet supported",
        )));
    }
    let i64_ty = ctx.context.i64_type();
    let num_bytes = width / 8;
    let byte_offset = bit_offset / 8;
    let extracted = extract_int(ctx, payload, byte_offset, num_bytes, endian)?;
    let mask = mask_for_width(ctx, width);
    let masked_ext = ctx
        .builder
        .build_and(extracted, mask, "lit_ext_mask")
        .or_ice()?;
    let const_value = i64_ty.const_int(value as u64, false);
    let masked_lit = ctx
        .builder
        .build_and(const_value, mask, "lit_mask")
        .or_ice()?;
    ctx.builder
        .build_int_compare(IntPredicate::EQ, masked_ext, masked_lit, "lit_eq")
        .or_ice()
}

/// Compare a run of bytes at `bit_offset / 8` against an emitted
/// constant payload via `memcmp`. `bit_offset` is byte-aligned by
/// construction, since string segments don't carry sub-byte offsets.
fn emit_literal_bytes<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_offset: u64,
    bytes: &[u8],
) -> Result<IntValue<'ctx>, LlvmError> {
    if bytes.is_empty() {
        return Ok(true_i1(ctx));
    }
    let i8_ty = ctx.context.i8_type();
    let i32_ty = ctx.context.i32_type();
    let i64_ty = ctx.context.i64_type();
    let byte_offset = bit_offset / 8;
    let dest = unsafe {
        ctx.builder
            .build_in_bounds_gep(
                i8_ty,
                payload,
                &[i64_ty.const_int(byte_offset, false)],
                "str_pat_dst",
            )
            .or_ice()?
    };
    let lit_ptr = emit_string_literal_payload(ctx, bytes, "binpat_lit");
    let memcmp = declare_memcmp_extern(ctx);
    let cmp_result = ctx
        .call_basic(
            memcmp,
            &[
                dest.into(),
                lit_ptr.into(),
                i64_ty.const_int(bytes.len() as u64, false).into(),
            ],
            "str_pat_cmp",
        )?
        .into_int_value();
    ctx.builder
        .build_int_compare(
            IntPredicate::EQ,
            cmp_result,
            i32_ty.const_int(0, false),
            "str_pat_eq",
        )
        .or_ice()
}

/// Extract a sized integer at `bit_offset`, sign- or zero-extend
/// per the segment's modifier, narrow to the slot's declared type
/// (`Int8`..`Int64`/`UInt8`..`UInt64`), and store via the local
/// slot table. Returns nothing because bindings never gate the
/// arm. The length check + literal comparisons handle that.
#[allow(clippy::too_many_arguments)]
fn emit_bind_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_offset: u64,
    endian: BinaryEndian,
    local: IRLocalId,
    sign: BinarySign,
    ty: &IRType,
    width: u64,
) -> Result<(), LlvmError> {
    if !bit_offset.is_multiple_of(8) || !width.is_multiple_of(8) {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: sub-byte binary binding pattern segment (bit_offset={bit_offset}, \
             width={width}) is not yet supported",
        )));
    }
    let num_bytes = width / 8;
    let byte_offset = bit_offset / 8;
    let extracted = extract_int(ctx, payload, byte_offset, num_bytes, endian)?;
    let extended = extend_for_sign(ctx, extracted, sign, width)?;
    let narrowed = narrow_to_ir_type(ctx, extended, ty)?;
    let slot = ctx.local_slot(local);
    ctx.builder.build_store(slot, narrowed).or_ice().map(|_| ())
}

/// Allocate a fresh heap block sized to `8 + ceil(remaining_bits / 8)`
/// bytes, copy `remaining_bytes` from the subject payload, store
/// the bit-length header, and write the resulting payload pointer
/// into `local`'s slot (when present). Bit alignment is the caller's
/// responsibility. Typecheck enforces a byte-aligned prefix for
/// `: Binary` tails, and `: Bits` tails accept any prefix shape but
/// our lower path only emits byte-aligned `bit_offset`s through this
/// helper.
fn emit_greedy_tail<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_length: IntValue<'ctx>,
    byte_length: IntValue<'ctx>,
    bit_offset: u64,
    local: Option<IRLocalId>,
    ty: &IRType,
) -> Result<(), LlvmError> {
    if !bit_offset.is_multiple_of(8) {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: sub-byte binary greedy-tail segment (bit_offset={bit_offset}) is \
             not yet supported",
        )));
    }
    let Some(local) = local else {
        // `_: Binary` / `_: Bits`: successful length check already
        // covered the "rest exists" requirement, so no work to do.
        return Ok(());
    };
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let prefix_bytes = bit_offset / 8;
    let prefix_bits = bit_offset;
    let prefix_bytes_const = i64_ty.const_int(prefix_bytes, false);
    let prefix_bits_const = i64_ty.const_int(prefix_bits, false);

    let remaining_bytes = ctx
        .builder
        .build_int_sub(byte_length, prefix_bytes_const, "tail_bytes")
        .or_ice()?;
    let remaining_bits = ctx
        .builder
        .build_int_sub(bit_length, prefix_bits_const, "tail_bits")
        .or_ice()?;

    let alloc_size = block_alloc_size(ctx, remaining_bytes, false, "tail_alloc_size")?;
    let malloc = declare_malloc_extern(ctx);
    let base = ctx
        .call_basic(malloc, &[alloc_size.into()], "tail_alloc")?
        .into_pointer_value();
    let tail_payload = init_heap_block(ctx, base, remaining_bits, "tail")?;
    let src = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, payload, &[prefix_bytes_const], "tail_src")
            .or_ice()?
    };
    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[tail_payload.into(), src.into(), remaining_bytes.into()],
            "tail_cpy",
        )
        .or_ice()?;
    let _ = ty;
    let slot = ctx.local_slot(local);
    ctx.builder
        .build_store(slot, tail_payload)
        .or_ice()
        .map(|_| ())
}

/// Read `num_bytes` from `payload + byte_offset` and assemble them
/// into an `i64`. Mirrors v1's `extract_segment_value` byte-shift
/// loop. `Big` packs high-byte-first, `Little` low-byte-first.
fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    byte_offset: u64,
    num_bytes: u64,
    endian: BinaryEndian,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let mut result = i64_ty.const_int(0, false);
    let is_little = matches!(endian, BinaryEndian::Little);
    for i in 0..num_bytes {
        let ptr = unsafe {
            ctx.builder
                .build_in_bounds_gep(
                    i8_ty,
                    payload,
                    &[i64_ty.const_int(byte_offset + i, false)],
                    "seg_byte_ptr",
                )
                .or_ice()?
        };
        let byte_val = ctx
            .builder
            .build_load(i8_ty, ptr, "seg_byte")
            .or_ice()?
            .into_int_value();
        let extended = ctx
            .builder
            .build_int_z_extend(byte_val, i64_ty, "seg_ext")
            .or_ice()?;
        let shift_amount = if is_little {
            i * 8
        } else {
            (num_bytes - 1 - i) * 8
        };
        let shifted = if shift_amount > 0 {
            ctx.builder
                .build_left_shift(extended, i64_ty.const_int(shift_amount, false), "seg_shl")
                .or_ice()?
        } else {
            extended
        };
        result = ctx.builder.build_or(result, shifted, "seg_or").or_ice()?;
    }
    Ok(result)
}

/// Sign-extend the low `width` bits when `sign == Signed`. Fixes
/// v1's behavior of returning the raw extracted `i64` regardless
/// of `signed` / `unsigned` modifier. For example, the byte `0xFF`
/// in a `signed`-modified segment should bind as `-1`, not `255`.
fn extend_for_sign<'ctx>(
    ctx: &EmitContext<'ctx>,
    extracted: IntValue<'ctx>,
    sign: BinarySign,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    if !matches!(sign, BinarySign::Signed) || width == 0 || width >= 64 {
        return Ok(extracted);
    }
    let i64_ty = ctx.context.i64_type();
    let shift = i64_ty.const_int(64 - width, false);
    let shl = ctx
        .builder
        .build_left_shift(extracted, shift, "sign_shl")
        .or_ice()?;
    ctx.builder
        .build_right_shift(shl, shift, true, "sign_ashr")
        .or_ice()
}

/// Truncate the running `i64` extraction to the LLVM type backing
/// `ty`. Width must already match `ty`'s natural size. The caller is
/// responsible for the typecheck-time width pairing (`x: Int16` ↔
/// `width == 16`, etc.).
fn narrow_to_ir_type<'ctx>(
    ctx: &EmitContext<'ctx>,
    extended: IntValue<'ctx>,
    ty: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let target = match ty {
        IRType::Int8 | IRType::UInt8 => ctx.context.i8_type(),
        IRType::Int16 | IRType::UInt16 => ctx.context.i16_type(),
        IRType::Int32 | IRType::UInt32 => ctx.context.i32_type(),
        IRType::Int64 | IRType::UInt64 => ctx.context.i64_type(),
        other => {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: binary pattern binding can't narrow into IR type `{other:?}`",
            )));
        }
    };
    if extended.get_type().get_bit_width() == target.get_bit_width() {
        return Ok(extended.into());
    }
    ctx.builder
        .build_int_truncate(extended, target, "bind_trunc")
        .or_ice()
        .map(Into::into)
}

/// `(1 << width) - 1` as an `i64` constant. Saturates to all-ones
/// for `width >= 64`, which is the natural `i64` overflow point.
fn mask_for_width<'ctx>(ctx: &EmitContext<'ctx>, width: u64) -> IntValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    if width >= 64 {
        i64_ty.const_all_ones()
    } else {
        i64_ty.const_int((1u64 << width) - 1, false)
    }
}

/// `i1 true`. Used as the per-segment success bit for segments
/// that never fail (`Discard`, `BindInt`, `GreedyTail`).
fn true_i1<'ctx>(ctx: &EmitContext<'ctx>) -> IntValue<'ctx> {
    ctx.context.bool_type().const_int(1, false)
}
