//! Eval round-trip coverage for `<<segments>>` binary literals.
//! Pin per-segment encoding (default 8-bit ints, sized ints with
//! big/little endian, type-annotated floats, string segments,
//! sub-byte segments -> Bits) byte-for-byte against the runtime
//! `Value::Binary` / `Value::Bits` shapes.

use koja_ir_eval::Value;

mod common;

use common::evaluate_script as evaluate;

#[test]
fn three_byte_segments_pack_in_source_order() {
    assert_eq!(
        evaluate("<<1, 2, 3>>\n").unwrap(),
        Value::binary(vec![1, 2, 3]),
    );
}

#[test]
fn empty_literal_evaluates_to_empty_binary() {
    assert_eq!(evaluate("<<>>\n").unwrap(), Value::binary(vec![]),);
}

#[test]
fn sixteen_bit_big_endian_segment_packs_msb_first() {
    // Default endianness is big — `0x00FF` packs as `[0x00, 0xFF]`.
    assert_eq!(
        evaluate("<<255::16>>\n").unwrap(),
        Value::binary(vec![0x00, 0xFF]),
    );
}

#[test]
fn sixteen_bit_little_endian_segment_packs_lsb_first() {
    // `0x00FF` little-endian packs as `[0xFF, 0x00]`.
    assert_eq!(
        evaluate("<<255::16 little>>\n").unwrap(),
        Value::binary(vec![0xFF, 0x00]),
    );
}

#[test]
fn float32_big_endian_segment_packs_ieee_bytes() {
    // 1.0_f32 = 0x3F800000 in IEEE 754. Big-endian -> high byte
    // first.
    assert_eq!(
        evaluate("<<1.0: Float32>>\n").unwrap(),
        Value::binary(vec![0x3F, 0x80, 0x00, 0x00]),
    );
}

#[test]
fn string_segment_packs_utf8_payload() {
    assert_eq!(
        evaluate("<<\"hi\">>\n").unwrap(),
        Value::binary(vec![b'h', b'i']),
    );
}

#[test]
fn type_annotated_int_segment_uses_named_width() {
    // `: Int16` -> 16-bit big-endian, so 511 = 0x01FF -> [0x01, 0xFF].
    assert_eq!(
        evaluate("<<511: Int16>>\n").unwrap(),
        Value::binary(vec![0x01, 0xFF]),
    );
}

#[test]
fn sub_byte_literal_evaluates_to_bits() {
    // `<<5::3, 9::4>>` -> 5 (`101`) | 9 (`1001`) = `1011001 0` (the
    // trailing low bit of byte 0 is unused / zero-padded).
    // Total 7 bits, single byte: `0b1011 0010` = 0xB2.
    assert_eq!(
        evaluate("<<5::3, 9::4>>\n").unwrap(),
        Value::bits(vec![0xB2], 7),
    );
}

#[test]
fn byte_aligned_sub_byte_segments_evaluate_to_binary() {
    // `<<5::3, 9::5>>` total = 8 bits -> byte-aligned -> Binary.
    // `5` (`101`) packs into top 3 bits, `9` (`01001`) into low 5,
    // making `1010 1001` = 0xA9.
    assert_eq!(
        evaluate("<<5::3, 9::5>>\n").unwrap(),
        Value::binary(vec![0xA9]),
    );
}
