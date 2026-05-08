//! Lowering coverage for `<<segments>>` binary literals.
//!
//! Every literal lowers to a single
//! [`IRInstruction::BinaryConstruct`] whose `layout.total_bits`
//! matches the sum of segment widths and whose `segments` carry
//! per-segment `bit_offset`s in source order.

use expo_alpha_ir::{
    BinaryEndian, IRFunction, IRInstruction, IRType, LoweredBinarySegment, ResolvedBinaryLayout,
};
use expo_ast::util::dedent;

mod common;

use common::{function, lower_program_source as lower};

fn first_construct(function: &IRFunction) -> &IRInstruction {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find(|i| matches!(i, IRInstruction::BinaryConstruct { .. }))
        .expect("function should contain at least one BinaryConstruct instruction")
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
    let source = "
        fn main -> Binary
          <<1, 2, 3>>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (layout, segments) = unpack(first_construct(main));

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
    assert_eq!(main.return_type, IRType::Binary);
}

#[test]
fn sized_integer_segment_carries_explicit_width() {
    let source = "
        fn main -> Binary
          <<255::16>>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (layout, segments) = unpack(first_construct(main));

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
    let source = "
        fn main -> Binary
          <<1.0: Float32>>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (layout, segments) = unpack(first_construct(main));

    assert_eq!(layout.total_bits, 32);
    assert!(matches!(
        segments[0],
        LoweredBinarySegment::Float { width: 32, .. }
    ));
}

#[test]
fn string_segment_carries_byte_length() {
    let source = "
        fn main -> Binary
          <<\"hi\">>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (layout, segments) = unpack(first_construct(main));

    assert_eq!(layout.total_bits, 16);
    assert!(layout.byte_aligned);
    let LoweredBinarySegment::String { byte_length, .. } = segments[0] else {
        panic!("expected String segment");
    };
    assert_eq!(byte_length, 2);
}

#[test]
fn sub_byte_literal_lowers_to_bits_with_running_offsets() {
    let source = "
        fn main -> Bits
          <<1::3, 2::4>>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (layout, segments) = unpack(first_construct(main));

    assert_eq!(layout.total_bits, 7);
    assert!(!layout.byte_aligned);
    let widths: Vec<u64> = segments.iter().map(|s| s.width()).collect();
    let offsets: Vec<u64> = segments.iter().map(|s| s.bit_offset()).collect();
    assert_eq!(widths, vec![3, 4]);
    assert_eq!(offsets, vec![0, 3]);
    assert_eq!(main.return_type, IRType::Bits);
}

#[test]
fn little_endian_modifier_propagates_to_ir() {
    let source = "
        fn main -> Binary
          <<256::16 little>>
        end
    ";
    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let (_, segments) = unpack(first_construct(main));

    let LoweredBinarySegment::Integer { endian, .. } = segments[0] else {
        panic!("expected Integer segment");
    };
    assert_eq!(endian, BinaryEndian::Little);
}
