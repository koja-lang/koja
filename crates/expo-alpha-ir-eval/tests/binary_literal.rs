//! Eval round-trip coverage for `<<segments>>` binary literals.
//! Pin per-segment encoding (default 8-bit ints, sized ints with
//! big/little endian, type-annotated floats, string segments,
//! sub-byte segments → Bits) byte-for-byte against the runtime
//! `Value::Binary` / `Value::Bits` shapes.

use expo_alpha_ir_eval::Value;

mod common;

use common::evaluate_program as evaluate;

#[test]
fn three_byte_segments_pack_in_source_order() {
    assert_eq!(
        evaluate("fn main -> Binary\n  <<1, 2, 3>>\nend\n").unwrap(),
        Value::Binary(vec![1, 2, 3]),
    );
}

#[test]
fn empty_literal_evaluates_to_empty_binary() {
    assert_eq!(
        evaluate("fn main -> Binary\n  <<>>\nend\n").unwrap(),
        Value::Binary(vec![]),
    );
}

#[test]
fn sixteen_bit_big_endian_segment_packs_msb_first() {
    // Default endianness is big — `0x00FF` packs as `[0x00, 0xFF]`.
    assert_eq!(
        evaluate("fn main -> Binary\n  <<255::16>>\nend\n").unwrap(),
        Value::Binary(vec![0x00, 0xFF]),
    );
}

#[test]
fn sixteen_bit_little_endian_segment_packs_lsb_first() {
    // `0x00FF` little-endian packs as `[0xFF, 0x00]`.
    assert_eq!(
        evaluate("fn main -> Binary\n  <<255::16 little>>\nend\n").unwrap(),
        Value::Binary(vec![0xFF, 0x00]),
    );
}

#[test]
fn float32_big_endian_segment_packs_ieee_bytes() {
    // 1.0_f32 = 0x3F800000 in IEEE 754. Big-endian → high byte
    // first.
    assert_eq!(
        evaluate("fn main -> Binary\n  <<1.0: Float32>>\nend\n").unwrap(),
        Value::Binary(vec![0x3F, 0x80, 0x00, 0x00]),
    );
}

#[test]
fn string_segment_packs_utf8_payload() {
    assert_eq!(
        evaluate("fn main -> Binary\n  <<\"hi\">>\nend\n").unwrap(),
        Value::Binary(vec![b'h', b'i']),
    );
}

#[test]
fn type_annotated_int_segment_uses_named_width() {
    // `: Int16` → 16-bit big-endian, so 511 = 0x01FF → [0x01, 0xFF].
    assert_eq!(
        evaluate("fn main -> Binary\n  <<511: Int16>>\nend\n").unwrap(),
        Value::Binary(vec![0x01, 0xFF]),
    );
}

#[test]
fn sub_byte_literal_evaluates_to_bits() {
    // `<<5::3, 9::4>>` → 5 (`101`) | 9 (`1001`) = `1011001 0` (the
    // trailing low bit of byte 0 is unused / zero-padded).
    // Total 7 bits, single byte: `0b1011 0010` = 0xB2.
    assert_eq!(
        evaluate("fn main -> Bits\n  <<5::3, 9::4>>\nend\n").unwrap(),
        Value::Bits {
            bytes: vec![0xB2],
            bit_length: 7,
        },
    );
}

#[test]
fn byte_aligned_sub_byte_segments_evaluate_to_binary() {
    // `<<5::3, 9::5>>` total = 8 bits → byte-aligned → Binary.
    // `5` (`101`) packs into top 3 bits, `9` (`01001`) into low 5,
    // making `1010 1001` = 0xA9.
    assert_eq!(
        evaluate("fn main -> Binary\n  <<5::3, 9::5>>\nend\n").unwrap(),
        Value::Binary(vec![0xA9]),
    );
}
