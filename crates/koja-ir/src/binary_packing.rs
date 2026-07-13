//! Shared bit-stream packing for `<<...>>` construction. The eval
//! interpreter packs segments at runtime and constant folding packs
//! them at compile time. Both must agree bit for bit with the
//! `__koja_pack_bits` runtime helper the LLVM backend calls, so the
//! packers live in one place.

use crate::types::BinaryEndian;

/// Pack the low `width` bits of `value` into `buffer` starting at
/// `start_bit`, byte-flipping per `endian`. The byte-aligned fast
/// path collapses to a per-byte write (mirrors the LLVM backend's
/// `emit_byte_packing` shape). The sub-byte path delegates to
/// [`pack_bits_into`] so integer and float segments share one
/// bit-stream packer.
pub fn pack_integer_segment(
    buffer: &mut [u8],
    value: u64,
    width: u64,
    endian: BinaryEndian,
    start_bit: u64,
) {
    if width == 0 {
        return;
    }
    if start_bit.is_multiple_of(8) && width.is_multiple_of(8) {
        let num_bytes = (width / 8) as usize;
        let start_byte = (start_bit / 8) as usize;
        for i in 0..num_bytes {
            let shift = match endian {
                BinaryEndian::Little => (i as u32) * 8,
                BinaryEndian::Big => ((num_bytes - 1 - i) as u32) * 8,
            };
            buffer[start_byte + i] = (value >> shift) as u8;
        }
        return;
    }
    // A sub-byte segment writes the low `width` bits MSB first,
    // mirroring the runtime `__koja_pack_bits` helper. Endianness is
    // meaningless for non-byte-multiple widths in v1, so we only
    // honour the high-order-first convention.
    pack_bits_into(buffer, value, width, start_bit);
}

/// Write the low `width` bits of `value` (MSB first) into `buffer`
/// at bit offset `start_bit`. `buffer` is assumed pre-zeroed, and we
/// `or` rather than overwrite so adjacent segments that share a
/// byte don't clobber each other.
pub fn pack_bits_into(buffer: &mut [u8], value: u64, width: u64, start_bit: u64) {
    for i in 0..width {
        let bit = ((value >> (width - 1 - i)) & 1) as u8;
        if bit == 0 {
            continue;
        }
        let bit_pos = start_bit + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        buffer[byte] |= 1 << bit_in_byte;
    }
}
