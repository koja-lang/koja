//! Binary-literal construction helpers for [`crate::Interp`] -- the
//! runtime side of [`expo_ir::IRInstruction::BinaryConstruct`].
//!
//! Walks the lowered segment list, materializing each operand against
//! the current frame, and packs the result into a `Vec<u8>` according
//! to the segment's resolved width / kind / endianness. Sub-byte
//! widths are rejected up front -- both lowering
//! ([`expo_ir::lower::binary::resolve_binary_segments`]) and the
//! codegen executor reject them too, so the interpreter mirrors the
//! same error shape rather than introducing a divergent surface.

use std::rc::Rc;

use expo_ast::ast::BinaryEndianness;
use expo_ir::resolved::construction::{ResolvedBinaryLayout, ResolvedBinarySegmentKind};
use expo_ir::values::LoweredBinarySegment;

use crate::error::RuntimeError;
use crate::frame::Frame;
use crate::value::Value;

/// Build a [`Value::Binary`] from the lowered segment list. Each
/// segment is materialized against `frame`, then packed according to
/// its `kind` -- `String` segments are byte-copied verbatim, integer
/// segments are width- and endianness-extracted from the
/// frame-side numeric value, and float segments are bit-cast to
/// integers and packed big-endian (matching `compile_binary_literal`'s
/// long-standing behavior).
pub(crate) fn construct_binary(
    frame: &Frame,
    layout: &ResolvedBinaryLayout,
    segments: &[LoweredBinarySegment],
) -> Result<Value, RuntimeError> {
    let total_bytes = (layout.total_bits / 8) as usize;
    let mut buf = Vec::with_capacity(total_bytes);

    for segment in segments {
        if !segment.bit_width.is_multiple_of(8) {
            return Err(RuntimeError::Unsupported(format!(
                "binary literal: sub-byte segment ({} bits) not supported",
                segment.bit_width
            )));
        }
        let num_bytes = (segment.bit_width / 8) as usize;
        let value = frame.materialize(&segment.value)?;
        match segment.kind {
            ResolvedBinarySegmentKind::String => pack_string(&mut buf, num_bytes, value)?,
            ResolvedBinarySegmentKind::Float => pack_float(&mut buf, num_bytes, value)?,
            ResolvedBinarySegmentKind::Integer { endianness } => {
                pack_integer(&mut buf, num_bytes, endianness, value)?;
            }
        }
    }

    Ok(Value::Binary(Rc::new(buf)))
}

/// Append `value`'s String payload to `buf`. Lowering's
/// `string_segment_bit_width` sets the segment's `bit_width` to
/// `byte_len * 8` from the same `&str`, so `num_bytes` and the
/// payload length are equal by construction; we only sanity-check
/// the value variant.
fn pack_string(buf: &mut Vec<u8>, _num_bytes: usize, value: Value) -> Result<(), RuntimeError> {
    let Value::String(s) = value else {
        return Err(RuntimeError::TypeMismatch(format!(
            "binary segment: expected Value::String, got {value:?}"
        )));
    };
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Pack a `Value::Float` / `Value::Float32` into `num_bytes` (4 or 8)
/// big-endian bytes via `to_bits()` -- mirrors codegen's
/// `bit_cast<f32, i32>` / `bit_cast<f64, i64>` + big-endian packing.
fn pack_float(buf: &mut Vec<u8>, num_bytes: usize, value: Value) -> Result<(), RuntimeError> {
    match (num_bytes, &value) {
        (4, Value::Float(f)) => buf.extend_from_slice(&(*f as f32).to_bits().to_be_bytes()),
        (4, Value::Float32(f)) => buf.extend_from_slice(&f.to_bits().to_be_bytes()),
        (8, Value::Float(f)) => buf.extend_from_slice(&f.to_bits().to_be_bytes()),
        (8, Value::Float32(f)) => buf.extend_from_slice(&(*f as f64).to_bits().to_be_bytes()),
        (n, _) => {
            return Err(RuntimeError::Unsupported(format!(
                "binary float segment: unsupported width {n} bytes / value {value:?}"
            )));
        }
    }
    Ok(())
}

/// Pack `value` (any integer-shaped [`Value`]) into `num_bytes` of the
/// given endianness, matching the codegen `emit_byte_packing` shift
/// loop (which truncates to `num_bytes` bytes of the value's i64
/// representation).
fn pack_integer(
    buf: &mut Vec<u8>,
    num_bytes: usize,
    endianness: BinaryEndianness,
    value: Value,
) -> Result<(), RuntimeError> {
    let i = value.as_int().ok_or_else(|| {
        RuntimeError::TypeMismatch(format!(
            "binary integer segment: expected integer Value, got {value:?}"
        ))
    })?;
    let bytes = match endianness {
        BinaryEndianness::Little => {
            let all = (i as u64).to_le_bytes();
            all[..num_bytes].to_vec()
        }
        BinaryEndianness::Big => {
            let all = (i as u64).to_be_bytes();
            all[8 - num_bytes..].to_vec()
        }
    };
    buf.extend_from_slice(&bytes);
    Ok(())
}
