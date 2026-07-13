//! Lowering coverage for `<<segments>>` binary literals.
//!
//! A pure fixed-width literal lowers to a single
//! [`IRInstruction::BinaryConstruct`] whose `layout.total_bits`
//! matches the sum of segment widths and whose `segments` carry
//! per-segment `bit_offset`s in source order. `Binary` splices
//! desugar into per-run constructs joined by
//! [`IRInstruction::Concat`]. No splice-specific IR exists.

use koja_ir::{
    BinaryEndian, ConcatKind, IRBasicBlock, IRInstruction, IRType, LoweredBinarySegment,
    ResolvedBinaryLayout,
};

mod common;

use common::lower_script_source as lower;

fn first_construct(blocks: &[IRBasicBlock]) -> &IRInstruction {
    blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find(|i| matches!(i, IRInstruction::BinaryConstruct { .. }))
        .expect("body should contain at least one BinaryConstruct instruction")
}

fn count_matching(blocks: &[IRBasicBlock], predicate: impl Fn(&IRInstruction) -> bool) -> usize {
    blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|inst| predicate(inst))
        .count()
}

fn unpack(inst: &IRInstruction) -> (ResolvedBinaryLayout, &[LoweredBinarySegment]) {
    let IRInstruction::BinaryConstruct {
        layout, segments, ..
    } = inst
    else {
        unreachable!()
    };
    (*layout, segments)
}

#[test]
fn three_default_byte_segments_lay_out_at_byte_offsets() {
    let script = lower("<<1, 2, 3>>\n");
    let (layout, segments) = unpack(first_construct(&script.blocks));

    assert_eq!(layout.total_bits, 24);
    assert!(layout.byte_aligned);
    assert_eq!(segments.len(), 3);
    let offsets: Vec<u64> = segments.iter().map(|s| s.bit_offset()).collect();
    assert_eq!(offsets, vec![0, 8, 16]);
    for seg in segments {
        let LoweredBinarySegment::Integer { width, endian, .. } = seg else {
            panic!("expected Integer segment, got {seg:?}");
        };
        assert_eq!(*width, 8);
        assert_eq!(*endian, BinaryEndian::Big);
    }
    assert_eq!(script.return_type, IRType::Binary);
}

#[test]
fn sized_integer_segment_carries_explicit_width() {
    let script = lower("<<255::16>>\n");
    let (layout, segments) = unpack(first_construct(&script.blocks));

    assert_eq!(layout.total_bits, 16);
    assert!(layout.byte_aligned);
    assert_eq!(segments.len(), 1);
    let LoweredBinarySegment::Integer { width, .. } = segments[0] else {
        panic!("expected Integer segment");
    };
    assert_eq!(width, 16);
}

#[test]
fn float32_segment_lowers_as_float() {
    let script = lower("<<1.0: Float32>>\n");
    let (layout, segments) = unpack(first_construct(&script.blocks));

    assert_eq!(layout.total_bits, 32);
    assert!(matches!(
        segments[0],
        LoweredBinarySegment::Float { width: 32, .. }
    ));
}

#[test]
fn string_segment_carries_byte_length() {
    let script = lower("<<\"hi\">>\n");
    let (layout, segments) = unpack(first_construct(&script.blocks));

    assert_eq!(layout.total_bits, 16);
    assert!(layout.byte_aligned);
    let LoweredBinarySegment::String { byte_length, .. } = segments[0] else {
        panic!("expected String segment");
    };
    assert_eq!(byte_length, 2);
}

#[test]
fn sub_byte_literal_lowers_to_bits_with_running_offsets() {
    let script = lower("<<1::3, 2::4>>\n");
    let (layout, segments) = unpack(first_construct(&script.blocks));

    assert_eq!(layout.total_bits, 7);
    assert!(!layout.byte_aligned);
    let widths: Vec<u64> = segments.iter().map(|s| s.width()).collect();
    let offsets: Vec<u64> = segments.iter().map(|s| s.bit_offset()).collect();
    assert_eq!(widths, vec![3, 4]);
    assert_eq!(offsets, vec![0, 3]);
    assert_eq!(script.return_type, IRType::Bits);
}

#[test]
fn little_endian_modifier_propagates_to_ir() {
    let script = lower("<<256::16 little>>\n");
    let (_, segments) = unpack(first_construct(&script.blocks));

    let LoweredBinarySegment::Integer { endian, .. } = segments[0] else {
        panic!("expected Integer segment");
    };
    assert_eq!(endian, BinaryEndian::Little);
}

#[test]
fn splice_desugars_to_run_constructs_joined_by_concat() {
    // `<<0x51::8, b, 0x02::8>>` splits into two one-byte runs around
    // the splice. Three constructs total (one is `b`'s own literal),
    // two binary concats, and no splice-specific instruction.
    let script = lower("b = <<1, 2>>\n  <<0x51::8, b, 0x02::8>>\n");

    let constructs = count_matching(&script.blocks, |i| {
        matches!(i, IRInstruction::BinaryConstruct { .. })
    });
    let concats = count_matching(&script.blocks, |i| {
        matches!(
            i,
            IRInstruction::Concat {
                kind: ConcatKind::Binary,
                ..
            }
        )
    });
    assert_eq!(constructs, 3);
    assert_eq!(concats, 2);
    assert_eq!(script.return_type, IRType::Binary);
}

#[test]
fn splice_only_literal_clones_instead_of_concatenating() {
    // `<<b>>` has no static run, so the lone spliced value is cloned
    // into an owned result. No concat is emitted.
    let script = lower("b = <<1>>\n  <<b>>\n");

    let constructs = count_matching(&script.blocks, |i| {
        matches!(i, IRInstruction::BinaryConstruct { .. })
    });
    let concats = count_matching(&script.blocks, |i| {
        matches!(i, IRInstruction::Concat { .. })
    });
    let clones = count_matching(&script.blocks, |i| matches!(i, IRInstruction::Clone { .. }));
    assert_eq!(constructs, 1, "only `b`'s own literal constructs");
    assert_eq!(concats, 0);
    assert_eq!(clones, 1);
    assert_eq!(script.return_type, IRType::Binary);
}
