//! Lowering for `<<segments...>>` binary literal layout.
//!
//! Computes per-segment kind (string/float/integer with endianness) and
//! byte-aligned bit width without touching LLVM. Emission then packs the
//! values byte-by-byte according to the [`ResolvedBinaryLayout`].

use expo_ast::ast::{
    BinaryEndianness, BinarySegment, BinaryUnit, ExprKind, Literal, StringPart, TypeExpr,
};
use expo_typecheck::types::Primitive;

use crate::resolved::construction::{
    ResolvedBinaryLayout, ResolvedBinarySegment, ResolvedBinarySegmentKind,
};

/// Resolves the layout of a binary literal by computing each segment's
/// kind and bit width. Validates that all segments are byte-aligned.
pub fn resolve_binary_segments(segments: &[BinarySegment]) -> Result<ResolvedBinaryLayout, String> {
    let mut resolved = Vec::with_capacity(segments.len());
    let mut total_bits: u64 = 0;

    for seg in segments {
        let bits = segment_bit_width(seg)?;
        if !bits.is_multiple_of(8) {
            return Err(format!(
                "sub-byte segment ({bits} bits) not yet supported in codegen"
            ));
        }
        total_bits += bits;

        let kind = if string_segment_bit_width(seg).is_some() {
            ResolvedBinarySegmentKind::String
        } else if is_float_segment(seg) {
            ResolvedBinarySegmentKind::Float
        } else {
            let endianness = seg.endianness.unwrap_or(BinaryEndianness::Big);
            ResolvedBinarySegmentKind::Integer { endianness }
        };

        resolved.push(ResolvedBinarySegment {
            bit_width: bits,
            kind,
        });
    }

    Ok(ResolvedBinaryLayout {
        segments: resolved,
        total_bits,
    })
}

/// Resolves a segment's bit width from its AST specification. Returns an
/// error for dynamic (non-literal) sizes which are not yet supported.
pub fn segment_bit_width(seg: &BinarySegment) -> Result<u64, String> {
    if let Some(bit_width) = string_segment_bit_width(seg) {
        return Ok(bit_width);
    }
    if let Some(size_expr) = &seg.size {
        if let ExprKind::Literal {
            value: Literal::Int(n),
            ..
        } = &size_expr.kind
        {
            let size = n
                .parse::<u64>()
                .map_err(|_| format!("invalid segment size: {n}"))?;
            if seg.unit == BinaryUnit::Byte {
                Ok(size * 8)
            } else {
                Ok(size)
            }
        } else {
            Err("dynamic segment sizes not yet supported in codegen".to_string())
        }
    } else if let Some(type_ann) = &seg.type_ann {
        type_ann_bit_width(type_ann)
            .ok_or_else(|| "unknown type annotation in binary segment".to_string())
    } else {
        Ok(8)
    }
}

/// Returns the bit width of a string literal segment, or `None` if the
/// segment is not a plain string literal (interpolations are not
/// supported in binary segments).
pub fn string_segment_bit_width(seg: &BinarySegment) -> Option<u64> {
    if let ExprKind::String { parts, .. } = &seg.value.kind {
        let byte_len: u64 = parts
            .iter()
            .map(|p| match p {
                StringPart::Literal { value, .. } => value.len() as u64,
                _ => 0,
            })
            .sum();
        Some(byte_len * 8)
    } else {
        None
    }
}

/// Resolves a `Type` annotation in a binary segment to its bit width.
pub fn type_ann_bit_width(type_ann: &TypeExpr) -> Option<u64> {
    if let TypeExpr::Named { path, .. } = type_ann {
        let name = path.last()?;
        Primitive::from_name(name).and_then(|p| p.bit_width())
    } else {
        None
    }
}

/// True when the segment's type annotation is `Float32` or `Float64`.
pub fn is_float_segment(seg: &BinarySegment) -> bool {
    if let Some(TypeExpr::Named { path, .. }) = &seg.type_ann
        && let Some(name) = path.last()
    {
        return matches!(name.as_str(), "Float32" | "Float64");
    }
    false
}
