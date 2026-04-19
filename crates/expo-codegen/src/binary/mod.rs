//! Binary/Bits codegen: construction (`<<segments...>>` expressions),
//! pattern matching (binary patterns in `match` arms), and shared segment
//! helpers used by both.

pub(crate) mod construction;
pub(crate) mod patterns;

pub(crate) use expo_ir::lower::binary::{
    is_float_segment, segment_bit_width, string_segment_bit_width,
};
