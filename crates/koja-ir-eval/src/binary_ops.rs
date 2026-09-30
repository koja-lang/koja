//! Byte and bit operations on `Binary`, `Bits`, and `String` values:
//! the `<>` concat, `<<segments>>` literal construction, and the
//! `BinaryMatch` driver. Each mirrors the LLVM backend's emission so
//! both targets agree byte for byte. Frame access is limited to
//! value lookup and local binds.

use std::rc::Rc;

use koja_ir::{
    BinaryEndian, BinarySign, ConcatKind, IRType, LoweredBinaryMatchLayout, LoweredBinaryPattern,
    LoweredBinarySegment, ResolvedBinaryLayout, pack_integer_segment,
};

use crate::error::RuntimeError;
use crate::interpreter::{Frame, lookup};
use crate::value::Value;

/// Append `length` bits from `src` (which is left-aligned with
/// `length` valid bits and possible zero padding in the low bits of
/// its trailing byte) into `dest` starting at bit offset
/// `start_bit`. Helper for [`concat_values`]'s `Bits` arm, mirroring
/// the LLVM `__koja_concat_bits` runtime helper.
fn append_bits(dest: &mut [u8], start_bit: u64, src: &[u8], length: u64) {
    if length == 0 {
        return;
    }
    let shift = (start_bit % 8) as u32;
    let dest_byte_start = (start_bit / 8) as usize;
    if shift == 0 {
        let src_bytes = length.div_ceil(8) as usize;
        dest[dest_byte_start..dest_byte_start + src_bytes].copy_from_slice(&src[..src_bytes]);
        return;
    }
    // Bit-shift each source byte right by `shift`, OR'd into the
    // current dest byte's low bits + the next dest byte's high
    // bits. Source padding bits past `length` are zero, so a spill
    // of them is harmless.
    let mut remaining = length;
    let mut src_idx = 0;
    let mut dest_idx = dest_byte_start;
    while remaining > 0 {
        let byte = src[src_idx];
        dest[dest_idx] |= byte >> shift;
        let next_bits = remaining.min(8);
        let consumed_in_low = next_bits + shift as u64;
        if consumed_in_low > 8 - shift as u64 && dest_idx + 1 < dest.len() {
            dest[dest_idx + 1] |= byte << (8 - shift);
        }
        if remaining > 8 {
            remaining -= 8;
            src_idx += 1;
            dest_idx += 1;
        } else {
            remaining = 0;
        }
    }
}

/// Apply `<>` to two heap-payload values. Mirrors the LLVM
/// backend's split: `String` / `Binary` are byte-aligned `memcpy`s,
/// `Bits` does sub-byte alignment in Rust (the runtime helper's
/// algorithm). Mismatched [`Value`] kinds vs `kind` surface a
/// defensive `TypeMismatch`, since seal + typecheck should have
/// kept these consistent.
pub(crate) fn concat_values(
    kind: ConcatKind,
    left: &Value,
    right: &Value,
) -> Result<Value, RuntimeError> {
    match kind {
        ConcatKind::String => {
            let (Value::String(l), Value::String(r)) = (left, right) else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<String> on `{left}` and `{right}`"),
                });
            };
            let mut out = Vec::with_capacity(l.len() + r.len());
            out.extend_from_slice(l);
            out.extend_from_slice(r);
            Ok(Value::string(out))
        }
        ConcatKind::Binary => {
            let (Value::Binary(l), Value::Binary(r)) = (left, right) else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<Binary> on `{left}` and `{right}`"),
                });
            };
            let mut out = Vec::with_capacity(l.len() + r.len());
            out.extend_from_slice(l);
            out.extend_from_slice(r);
            Ok(Value::binary(out))
        }
        ConcatKind::Bits => {
            let (
                Value::Bits {
                    bytes: lb,
                    bit_length: ll,
                },
                Value::Bits {
                    bytes: rb,
                    bit_length: rl,
                },
            ) = (left, right)
            else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<Bits> on `{left}` and `{right}`"),
                });
            };
            let total = ll + rl;
            let total_bytes = total.div_ceil(8) as usize;
            let mut out = vec![0u8; total_bytes];
            // Copy lhs bits (which are already left-aligned in `lb`)
            // verbatim. The trailing partial byte already has its
            // high bits set and low bits zeroed.
            for (idx, byte) in lb.iter().enumerate() {
                out[idx] = *byte;
            }
            // Append rhs bits starting at bit offset `ll`.
            append_bits(&mut out, *ll, rb, *rl);
            Ok(Value::bits(out, total))
        }
    }
}

/// Build a `<<segments>>` literal as a runtime [`Value::Binary`] (when
/// `layout.byte_aligned`) or [`Value::Bits`] (otherwise). Segments
/// are packed in source order at their pre-computed `bit_offset`s.
/// Integer and float bytes get endian-shuffled, string segments
/// `memcpy` their payload, and sub-byte segments funnel through the
/// shared [`pack_integer_segment`] bit packer. The buffer is
/// pre-zeroed so unused trailing bits in the last byte stay zero.
pub(crate) fn construct_binary_literal(
    layout: ResolvedBinaryLayout,
    segments: &[LoweredBinarySegment],
    frame: &Frame,
) -> Result<Value, RuntimeError> {
    let total_bytes = layout.total_bits.div_ceil(8) as usize;
    let mut buffer = vec![0u8; total_bytes];

    for segment in segments {
        match segment {
            LoweredBinarySegment::Integer {
                value,
                width,
                endian,
                bit_offset,
                ..
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let int_value = match resolved {
                    Value::Int(n) => n as u64,
                    other => {
                        return Err(RuntimeError::TypeMismatch {
                            detail: format!(
                                "binary literal integer segment expected an Int value, got {other}",
                            ),
                        });
                    }
                };
                pack_integer_segment(&mut buffer, int_value, *width, *endian, *bit_offset);
            }
            LoweredBinarySegment::Float {
                value,
                width,
                endian,
                bit_offset,
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let bits: u64 = match (*width, &resolved) {
                    (32, Value::Float32(v)) => u64::from(v.to_bits()),
                    (32, Value::Float64(v)) => u64::from((*v as f32).to_bits()),
                    (64, Value::Float64(v)) => v.to_bits(),
                    (64, Value::Float32(v)) => f64::from(*v).to_bits(),
                    (w, _) => panic!(
                        "interpreter: BinaryConstruct float segment of width {w}, \
                         but float widths are 32 or 64 (seal invariant violation)",
                    ),
                };
                pack_integer_segment(&mut buffer, bits, *width, *endian, *bit_offset);
            }
            LoweredBinarySegment::String {
                value,
                byte_length,
                bit_offset,
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let Value::String(bytes) = resolved else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!(
                            "binary literal string segment expected a String value, got {resolved}",
                        ),
                    });
                };
                debug_assert!(
                    bytes.len() as u64 >= *byte_length,
                    "interpreter: BinaryConstruct string segment carries byte_length {byte_length} \
                     but the runtime String holds {} bytes (typecheck/lower invariant violation)",
                    bytes.len(),
                );
                let start_byte = (bit_offset / 8) as usize;
                buffer[start_byte..start_byte + *byte_length as usize]
                    .copy_from_slice(&bytes[..*byte_length as usize]);
            }
        }
    }

    if layout.byte_aligned {
        Ok(Value::binary(buffer))
    } else {
        Ok(Value::bits(buffer, layout.total_bits))
    }
}

/// Eval-side `BinaryMatch` driver, mirroring the LLVM emission
/// described on [`koja_ir::IRInstruction::BinaryMatch`]: gate on the
/// subject's runtime bit length (equality without a greedy tail,
/// `>=` with one), then test every literal segment, extracting each
/// `BindInt` / `GreedyTail` slice into its pre-declared local slot
/// as a side effect. Binds happen as segments are walked, matching
/// the LLVM order. A later literal failure leaves earlier binds
/// written, which is unobservable because the arm body only runs
/// when the whole match succeeds.
pub(crate) fn execute_binary_match(
    layout: LoweredBinaryMatchLayout,
    segments: &[LoweredBinaryPattern],
    subject: &Value,
    frame: &mut Frame,
) -> Result<bool, RuntimeError> {
    let (bytes, bit_length) = match subject {
        Value::Binary(b) | Value::String(b) => (b.as_slice(), b.len() as u64 * 8),
        Value::Bits { bytes, bit_length } => (bytes.as_slice(), *bit_length),
        other => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("binary match expects a Binary/Bits/String subject, got {other}"),
            });
        }
    };
    let length_ok = if layout.has_greedy_tail {
        bit_length >= layout.fixed_bits
    } else {
        bit_length == layout.fixed_bits
    };
    if !length_ok {
        return Ok(false);
    }

    for segment in segments {
        match segment {
            LoweredBinaryPattern::LiteralInt {
                bit_offset,
                endian,
                sign: _,
                value,
                width,
            } => {
                // Compare raw width-truncated bits: a negative
                // signed literal and its two's-complement bit
                // pattern agree under the mask, so the sign
                // modifier doesn't change the test.
                if !literal_segment_matches(bytes, *width, *endian, *bit_offset, *value) {
                    return Ok(false);
                }
            }
            LoweredBinaryPattern::LiteralBytes {
                bit_offset,
                bytes: expected,
            } => {
                let start = (*bit_offset / 8) as usize;
                if bytes[start..start + expected.len()] != expected[..] {
                    return Ok(false);
                }
            }
            LoweredBinaryPattern::BindInt {
                bit_offset,
                endian,
                local,
                sign,
                ty: _,
                width,
            } => {
                let extracted = extract_integer_segment(bytes, *width, *endian, *bit_offset);
                frame
                    .locals
                    .insert(*local, Value::Int(sign_interpret(extracted, *width, *sign)));
            }
            LoweredBinaryPattern::Discard { .. } => {}
            LoweredBinaryPattern::GreedyTail {
                bit_offset,
                local,
                ty,
            } => {
                let Some(local) = local else { continue };
                let tail = match ty {
                    // Typecheck guarantees a byte-aligned prefix for
                    // a `Binary` tail.
                    IRType::Binary => Value::binary(&bytes[(*bit_offset / 8) as usize..]),
                    IRType::Bits => Value::bits(
                        extract_bit_range(bytes, *bit_offset, bit_length - *bit_offset),
                        bit_length - *bit_offset,
                    ),
                    other => panic!(
                        "interpreter: binary-match greedy tail typed `{other:?}`, \
                         but a tail is Binary or Bits (seal invariant violation)",
                    ),
                };
                frame.locals.insert(*local, tail);
            }
        }
    }
    Ok(true)
}

/// Append `right`'s bytes onto `left` in place when `left` is a
/// uniquely held `String` / `Binary` of the same kind. Hands `left`
/// back untouched otherwise, so the caller can fall back to the
/// copying concat.
pub(crate) fn extend_unique_bytes(mut left: Value, right: &Value) -> Result<Value, Value> {
    let (bytes, extra) = match (&mut left, right) {
        (Value::Binary(bytes), Value::Binary(extra))
        | (Value::String(bytes), Value::String(extra)) => (bytes, extra),
        _ => return Err(left),
    };
    let Some(unique) = Rc::get_mut(bytes) else {
        return Err(left);
    };
    unique.extend_from_slice(extra);
    Ok(left)
}

/// Copy `length` bits starting at `start_bit` into a fresh
/// MSB-first, zero-padded byte buffer (the greedy-tail extraction
/// for `Bits`). Byte-aligned starts take the `memcpy` fast path.
fn extract_bit_range(bytes: &[u8], start_bit: u64, length: u64) -> Vec<u8> {
    let byte_count = length.div_ceil(8) as usize;
    if start_bit.is_multiple_of(8) {
        let start = (start_bit / 8) as usize;
        let mut out = bytes[start..start + byte_count].to_vec();
        // Zero any trailing bits past `length` so equality on the
        // resulting `Bits` value stays well-defined.
        if !length.is_multiple_of(8) {
            let last = out.len() - 1;
            out[last] &= !(0xffu8 >> (length % 8));
        }
        return out;
    }
    let mut out = vec![0u8; byte_count];
    for i in 0..length {
        let bit_pos = start_bit + i;
        let bit = (bytes[(bit_pos / 8) as usize] >> (7 - (bit_pos % 8) as u32)) & 1;
        if bit != 0 {
            out[(i / 8) as usize] |= 1 << (7 - (i % 8) as u32);
        }
    }
    out
}

/// Inverse of [`pack_integer_segment`]: read `width` bits at
/// `start_bit` as an unsigned integer, byte-shuffled per `endian`
/// on the byte-aligned fast path, MSB-first on the sub-byte path
/// (where endianness is meaningless). Callers keep `width` at or
/// under 64.
fn extract_integer_segment(bytes: &[u8], width: u64, endian: BinaryEndian, start_bit: u64) -> u64 {
    if width == 0 {
        return 0;
    }
    if start_bit.is_multiple_of(8) && width.is_multiple_of(8) {
        let num_bytes = (width / 8) as usize;
        let start_byte = (start_bit / 8) as usize;
        let mut value = 0u64;
        for (i, byte) in bytes[start_byte..start_byte + num_bytes].iter().enumerate() {
            let shift = match endian {
                BinaryEndian::Little => (i as u32) * 8,
                BinaryEndian::Big => ((num_bytes - 1 - i) as u32) * 8,
            };
            value |= u64::from(*byte) << shift;
        }
        return value;
    }
    let mut value = 0u64;
    for i in 0..width {
        let bit_pos = start_bit + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        value = (value << 1) | u64::from((bytes[byte] >> bit_in_byte) & 1);
    }
    value
}

/// Whether the `width` bits at `start_bit` equal the low `width` bits
/// of `value`. Widths up to 64 go through [`extract_integer_segment`].
/// Wider literals compare byte by byte against the sign-extended
/// two's complement encoding of `value`, since no machine word holds
/// them.
fn literal_segment_matches(
    bytes: &[u8],
    width: u64,
    endian: BinaryEndian,
    start_bit: u64,
    value: i128,
) -> bool {
    if width <= 64 {
        let extracted = extract_integer_segment(bytes, width, endian, start_bit);
        return extracted == (value as u64) & width_mask(width);
    }
    let fill = if value < 0 { 0xFF } else { 0x00 };
    // The byte of `value` at `significance` places from the least
    // significant end, with sign fill past the 128-bit payload.
    let value_byte = |significance: u64| -> u8 {
        if significance >= 16 {
            fill
        } else {
            (value >> (significance * 8)) as u8
        }
    };
    if start_bit.is_multiple_of(8) && width.is_multiple_of(8) {
        let num_bytes = width / 8;
        let start_byte = (start_bit / 8) as usize;
        return (0..num_bytes).all(|i| {
            let significance = match endian {
                BinaryEndian::Little => i,
                BinaryEndian::Big => num_bytes - 1 - i,
            };
            bytes[start_byte + i as usize] == value_byte(significance)
        });
    }
    // A sub-byte offset or width walks the bits MSB-first, which is
    // the only order a sub-byte run can have.
    (0..width).all(|i| {
        let bit_pos = start_bit + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        let actual = (bytes[byte] >> bit_in_byte) & 1;
        let significance = width - 1 - i;
        let expected = (value_byte(significance / 8) >> (significance % 8)) & 1;
        actual == expected
    })
}

/// Reinterpret the raw `width`-bit pattern per the segment's sign
/// modifier: sign-extend when `Signed` and the sign bit is set,
/// zero-extend otherwise. Mirrors the LLVM emission's `sext`/`zext`
/// choice on `BindInt`.
fn sign_interpret(value: u64, width: u64, sign: BinarySign) -> i64 {
    match sign {
        BinarySign::Unsigned => value as i64,
        BinarySign::Signed => {
            if width == 0 || width >= 64 {
                return value as i64;
            }
            let sign_bit = 1u64 << (width - 1);
            if value & sign_bit != 0 {
                (value | !width_mask(width)) as i64
            } else {
                value as i64
            }
        }
    }
}

/// All-ones mask covering the low `width` bits (`u64::MAX` at 64+).
fn width_mask(width: u64) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}
