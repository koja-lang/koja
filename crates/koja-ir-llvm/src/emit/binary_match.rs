//! LLVM emission for `IRInstruction::BinaryMatch`, the `<<segments>>`
//! binary pattern matching instruction.
//!
//! Algorithm:
//!
//! 1. Load the subject's runtime bit length from `subject - 8`.
//! 2. Compare the bit length against `layout.fixed_bits` (`EQ` when
//!    there is no greedy tail, `UGE` when there is). A failed check
//!    short-circuits to `false` without touching the payload.
//! 3. Test phase. AND together every literal segment's comparison
//!    ([`LoweredBinaryPattern::LiteralInt`] / `LiteralBytes`
//!    against the constant). A failed test short-circuits to
//!    `false` before any binding side effect runs.
//! 4. Bind phase, on the fully matched path only.
//!    - [`LoweredBinaryPattern::BindInt`] extracts the bit slice,
//!      sign-extends when the modifier asks for it, narrows to the
//!      slot's declared LLVM type, and stores it via the local slot
//!      table.
//!    - [`LoweredBinaryPattern::GreedyTail`] allocates a fresh
//!      heap block of `8 + ceil(remaining_bits / 8)` bytes,
//!      copies the remaining bytes from the subject, stores the
//!      bit-length header, and writes the payload pointer into
//!      the binding slot (when there is one).
//!    - [`LoweredBinaryPattern::Discard`] is a no-op in both
//!      phases.
//!
//! An integer segment on byte boundaries reads whole bytes in its
//! byte order. Any other integer segment reads its bits most
//! significant first, which is the one order a run of bits has. The
//! interpreter reads both the same way.
//!
//! A greedy tail is gated to a byte-aligned `bit_offset`.
//! Typecheck rejects bit-misaligned `: Binary` tails, but a `Bits`
//! greedy tail with a byte-aligned fixed prefix and a sub-byte
//! suffix still flows through here. The emitter `memcpy`s
//! `ceil(remaining_bits / 8)` bytes and lets the heap layout carry
//! the exact bit count.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use koja_ir::{
    BinaryEndian, BinarySign, IRLocalId, IRType, LoweredBinaryMatchLayout, LoweredBinaryPattern,
    ValueId,
};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_malloc_extern, declare_memcmp_extern, declare_memcpy_extern};

use super::constants::emit_string_literal_payload;
use super::heap_layout::{
    Rounding, block_alloc_size, byte_count, init_heap_block, load_bit_length,
};
use super::{ValueMap, lookup};

/// Where an integer segment reads from: the subject payload, the
/// segment's bit position in it, and the byte order of the bytes it
/// covers. The literal compare and the binding extract share it and
/// differ only in what they do with the bytes.
#[derive(Clone, Copy)]
struct BitCursor<'ctx> {
    bit_offset: u64,
    endian: BinaryEndian,
    payload: PointerValue<'ctx>,
}

/// Lower an `IRInstruction::BinaryMatch`. Returns the `i1` success
/// bit. Binding segments stamp their extracted values into the
/// pre-declared local slots as a side effect, gated on the whole
/// pattern matching.
pub(super) fn emit_binary_match<'ctx>(
    ctx: &EmitContext<'ctx>,
    layout: LoweredBinaryMatchLayout,
    segments: &[LoweredBinaryPattern],
    subject: ValueId,
    values: &ValueMap<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let payload = lookup(values, subject).into_pointer_value();
    let bit_length = load_bit_length(ctx, payload, "bin_pat_bit_len")?;
    let length_ok = length_check(ctx, &layout, bit_length)?;

    // Segment extraction indexes off the subject length, so on a
    // too-short subject the reads run past the payload and the
    // greedy-tail size underflows to a huge `malloc`, which returns
    // null and then faults with SIGBUS.
    // Gate it behind the length check. A failed check short-circuits
    // to `false` without touching the payload.
    let entry_block = ctx
        .builder
        .get_insert_block()
        .expect("binary match emitted with no active block");
    let function = entry_block
        .get_parent()
        .expect("binary match active block has no parent function");
    let test_block = ctx.context.append_basic_block(function, "bin_pat_test");
    let bind_block = ctx.context.append_basic_block(function, "bin_pat_bind");
    let merge_block = ctx.context.append_basic_block(function, "bin_pat_merge");
    ctx.builder
        .build_conditional_branch(length_ok, test_block, merge_block)
        .or_ice()?;

    // The test phase runs literal comparisons only, no side effects.
    ctx.builder.position_at_end(test_block);
    let mut tests_ok = true_i1(ctx);
    for segment in segments {
        let segment_ok = emit_segment_test(ctx, payload, segment)?;
        tests_ok = ctx
            .builder
            .build_and(tests_ok, segment_ok, "bin_pat_and")
            .or_ice()?;
    }
    let test_end = ctx.builder.get_insert_block().unwrap_or(test_block);
    ctx.builder
        .build_conditional_branch(tests_ok, bind_block, merge_block)
        .or_ice()?;

    // The bind phase runs only when the whole pattern matched. The
    // greedy tail's fresh allocation must not happen on a failed
    // arm, or every miss leaks one block.
    ctx.builder.position_at_end(bind_block);
    for segment in segments {
        emit_segment_bind(ctx, payload, bit_length, segment)?;
    }
    let bind_end = ctx.builder.get_insert_block().unwrap_or(bind_block);
    ctx.builder
        .build_unconditional_branch(merge_block)
        .or_ice()?;

    ctx.builder.position_at_end(merge_block);
    let result = ctx
        .builder
        .build_phi(ctx.context.bool_type(), "bin_pat_result")
        .or_ice()?;
    let failed = ctx.context.bool_type().const_int(0, false);
    let matched = true_i1(ctx);
    result.add_incoming(&[
        (&failed, entry_block),
        (&failed, test_end),
        (&matched, bind_end),
    ]);
    Ok(result.as_basic_value().into_int_value())
}

/// `bit_length == fixed_bits` (exact match) when the pattern has no
/// greedy tail, `bit_length >= fixed_bits` (prefix match) when it
/// does. The check counts bits, as the interpreter does. A pattern
/// whose segments do not fill whole bytes matches a `Bits` subject
/// of that exact length, and no segment reads past the payload.
fn length_check<'ctx>(
    ctx: &EmitContext<'ctx>,
    layout: &LoweredBinaryMatchLayout,
    bit_length: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let pred = if layout.has_greedy_tail {
        IntPredicate::UGE
    } else {
        IntPredicate::EQ
    };
    ctx.builder
        .build_int_compare(
            pred,
            bit_length,
            i64_ty.const_int(layout.fixed_bits, false),
            "bin_pat_len_ok",
        )
        .or_ice()
}

/// Emit a segment's side-effect-free match predicate. Returns an
/// `i1` that the caller ANDs into the running success bit. Binding
/// segments never gate the arm and answer `true` here. Their stores
/// happen in [`emit_segment_bind`], after the whole pattern matched.
fn emit_segment_test<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    segment: &LoweredBinaryPattern,
) -> Result<IntValue<'ctx>, LlvmError> {
    match segment {
        LoweredBinaryPattern::LiteralInt {
            bit_offset,
            endian,
            value,
            width,
            ..
        } => {
            let cursor = BitCursor {
                bit_offset: *bit_offset,
                endian: *endian,
                payload,
            };
            emit_literal_int(ctx, cursor, *value, *width)
        }
        LoweredBinaryPattern::LiteralBytes { bit_offset, bytes } => {
            emit_literal_bytes(ctx, payload, *bit_offset, bytes)
        }
        LoweredBinaryPattern::BindInt { .. }
        | LoweredBinaryPattern::Discard { .. }
        | LoweredBinaryPattern::GreedyTail { .. } => Ok(true_i1(ctx)),
    }
}

/// Emit a segment's binding side effects (slot stores, the greedy
/// tail's fresh allocation). Runs only on the matched path. A failed
/// arm must not allocate, or every scan miss leaks the tail block.
fn emit_segment_bind<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_length: IntValue<'ctx>,
    segment: &LoweredBinaryPattern,
) -> Result<(), LlvmError> {
    match segment {
        LoweredBinaryPattern::LiteralInt { .. }
        | LoweredBinaryPattern::LiteralBytes { .. }
        | LoweredBinaryPattern::Discard { .. } => Ok(()),
        LoweredBinaryPattern::BindInt {
            bit_offset,
            endian,
            local,
            sign,
            ty,
            width,
        } => {
            let cursor = BitCursor {
                bit_offset: *bit_offset,
                endian: *endian,
                payload,
            };
            emit_bind_int(ctx, cursor, *local, *sign, ty, *width)
        }
        LoweredBinaryPattern::GreedyTail {
            bit_offset, local, ..
        } => emit_greedy_tail(ctx, payload, bit_length, *bit_offset, *local),
    }
}

/// Compare the `width` bits at `bit_offset` against the low `width`
/// bits of the constant `value`.
///
/// Widths past 64 bits do not fit the `i64` compare. On byte
/// boundaries they become a `memcmp` against the literal's encoded
/// bytes. Off byte boundaries they go through
/// [`emit_wide_literal_bits`].
fn emit_literal_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    cursor: BitCursor<'ctx>,
    value: i128,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let bit_offset = cursor.bit_offset;
    if width > 64 && on_byte_boundaries(bit_offset, width) {
        let bytes = encode_wide_literal(value, width / 8, cursor.endian);
        return emit_literal_bytes(ctx, cursor.payload, bit_offset, &bytes);
    }
    if width > 64 {
        return emit_wide_literal_bits(ctx, cursor, value, width);
    }
    let i64_ty = ctx.context.i64_type();
    let extracted = extract_segment(ctx, cursor, width)?;
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

/// Compare a literal wider than 64 bits that does not sit on byte
/// boundaries. The bits go most significant first, in runs of at
/// most 64, and every run must match.
fn emit_wide_literal_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    cursor: BitCursor<'ctx>,
    value: i128,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let mut all_eq = true_i1(ctx);
    let mut done = 0;
    while done < width {
        let run = (width - done).min(64);
        let expected = literal_run(value, width - done - run, run);
        let extracted = extract_bits(ctx, cursor.payload, cursor.bit_offset + done, run)?;
        let run_eq = ctx
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                extracted,
                i64_ty.const_int(expected, false),
                "lit_run_eq",
            )
            .or_ice()?;
        all_eq = ctx
            .builder
            .build_and(all_eq, run_eq, "lit_run_and")
            .or_ice()?;
        done += run;
    }
    Ok(all_eq)
}

/// The `run` bits of `value` whose lowest bit has the significance
/// `low`, as the low bits of a `u64`. Bits past the 128-bit payload
/// take the sign fill, so a negative literal stays negative at any
/// width.
fn literal_run(value: i128, low: u64, run: u64) -> u64 {
    (0..run).fold(0, |bits, i| {
        let significance = low + i;
        let set = if significance >= 128 {
            value < 0
        } else {
            (value >> significance) & 1 == 1
        };
        bits | (u64::from(set) << i)
    })
}

/// The two's complement encoding of `value` over `num_bytes` bytes in
/// `endian` order. Bytes past the 128-bit payload take the sign fill,
/// so a negative literal stays negative at any width.
fn encode_wide_literal(value: i128, num_bytes: u64, endian: BinaryEndian) -> Vec<u8> {
    let fill = if value < 0 { 0xFF } else { 0x00 };
    let value_byte = |significance: u64| -> u8 {
        if significance >= 16 {
            fill
        } else {
            (value >> (significance * 8)) as u8
        }
    };
    (0..num_bytes)
        .map(|i| {
            let significance = match endian {
                BinaryEndian::Little => i,
                BinaryEndian::Big => num_bytes - 1 - i,
            };
            value_byte(significance)
        })
        .collect()
}

/// Compare a run of bytes at `bit_offset / 8` against an emitted
/// constant payload via `memcmp`. `bit_offset` is byte-aligned by
/// construction, since string segments do not carry sub-byte offsets.
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
fn emit_bind_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    cursor: BitCursor<'ctx>,
    local: IRLocalId,
    sign: BinarySign,
    ty: &IRType,
    width: u64,
) -> Result<(), LlvmError> {
    let extracted = extract_segment(ctx, cursor, width)?;
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
/// the lower path only emits byte-aligned `bit_offset`s through this
/// helper.
///
/// `remaining_bytes` rounds up, so the tail of a `Bits` subject
/// keeps its partial last byte. That byte is the last byte of the
/// subject, whose unused bits are already zero.
fn emit_greedy_tail<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_length: IntValue<'ctx>,
    bit_offset: u64,
    local: Option<IRLocalId>,
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

    let remaining_bits = ctx
        .builder
        .build_int_sub(bit_length, prefix_bits_const, "tail_bits")
        .or_ice()?;
    let remaining_bytes = byte_count(ctx, remaining_bits, Rounding::Ceil, "tail_bytes")?;

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
    let slot = ctx.local_slot(local);
    ctx.builder
        .build_store(slot, tail_payload)
        .or_ice()
        .map(|_| ())
}

/// Read `num_bytes` at the cursor's byte-aligned position and
/// assemble them into an `i64` via a byte-shift loop. `Big` packs
/// high-byte-first, `Little` low-byte-first. The caller has already
/// checked that the cursor sits on a byte boundary.
fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    cursor: BitCursor<'ctx>,
    num_bytes: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let byte_offset = cursor.bit_offset / 8;
    let mut result = i64_ty.const_int(0, false);
    let is_little = matches!(cursor.endian, BinaryEndian::Little);
    for i in 0..num_bytes {
        let extended = load_byte(ctx, cursor.payload, byte_offset + i)?;
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

/// Read `width` bits, at most 64, at any bit position and return
/// them in the low bits of an `i64`. The bits go most significant
/// first, which is the one order a run of bits has.
///
/// The run can cover nine bytes. Each byte moves to its place in
/// the result, and the mask drops the bits before the run. The bits
/// after the run fall off in the right shift of the last byte.
fn extract_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    bit_offset: u64,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let first_byte = bit_offset / 8;
    let num_bytes = (bit_offset + width).div_ceil(8) - first_byte;
    let trailing_bits = num_bytes * 8 - bit_offset % 8 - width;
    let mut result = i64_ty.const_int(0, false);
    for i in 0..num_bytes {
        let extended = load_byte(ctx, payload, first_byte + i)?;
        let place = (num_bytes - 1 - i) * 8;
        let placed = if place > trailing_bits {
            let shift = i64_ty.const_int(place - trailing_bits, false);
            ctx.builder
                .build_left_shift(extended, shift, "bits_shl")
                .or_ice()?
        } else if place < trailing_bits {
            let shift = i64_ty.const_int(trailing_bits - place, false);
            ctx.builder
                .build_right_shift(extended, shift, false, "bits_lshr")
                .or_ice()?
        } else {
            extended
        };
        result = ctx.builder.build_or(result, placed, "bits_or").or_ice()?;
    }
    ctx.builder
        .build_and(result, mask_for_width(ctx, width), "bits_mask")
        .or_ice()
}

/// Read an integer segment of at most 64 bits. A segment on byte
/// boundaries goes through [`extract_int`], which honors its byte
/// order. Any other segment goes through [`extract_bits`].
fn extract_segment<'ctx>(
    ctx: &EmitContext<'ctx>,
    cursor: BitCursor<'ctx>,
    width: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    if on_byte_boundaries(cursor.bit_offset, width) {
        extract_int(ctx, cursor, width / 8)
    } else {
        extract_bits(ctx, cursor.payload, cursor.bit_offset, width)
    }
}

/// Load the payload byte at `byte_offset`, zero-extended to `i64`.
fn load_byte<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    byte_offset: u64,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let ptr = unsafe {
        ctx.builder
            .build_in_bounds_gep(
                i8_ty,
                payload,
                &[i64_ty.const_int(byte_offset, false)],
                "seg_byte_ptr",
            )
            .or_ice()?
    };
    let byte_val = ctx
        .builder
        .build_load(i8_ty, ptr, "seg_byte")
        .or_ice()?
        .into_int_value();
    ctx.builder
        .build_int_z_extend(byte_val, i64_ty, "seg_ext")
        .or_ice()
}

/// True when a segment starts on a byte boundary and fills whole
/// bytes.
fn on_byte_boundaries(bit_offset: u64, width: u64) -> bool {
    bit_offset.is_multiple_of(8) && width.is_multiple_of(8)
}

/// Sign-extend the low `width` bits when `sign == Signed`. Without
/// this, the raw extracted `i64` would ignore the `signed` /
/// `unsigned` modifier. For example, the byte `0xFF` in a
/// `signed`-modified segment should bind as `-1`, not `255`.
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
        other => panic!(
            "LLVM emit: binary pattern binding cannot narrow into IR type `{other:?}` (seal \
             invariant violation)",
        ),
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
