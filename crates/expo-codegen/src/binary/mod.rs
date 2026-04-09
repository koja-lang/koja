//! Binary/Bits codegen: construction (`<<segments...>>` expressions),
//! pattern matching (binary patterns in `match` arms), and shared segment
//! helpers used by both.

pub(crate) mod construction;
pub(crate) mod patterns;

use expo_ast::ast::{BinarySegment, BinaryUnit, ExprKind, Literal, StringPart, TypeExpr};
use expo_typecheck::types::Primitive;

/// Resolves a segment's bit width from its AST specification. Returns an error
/// for dynamic (non-literal) sizes which are not yet supported.
pub(crate) fn segment_bit_width(seg: &BinarySegment) -> Result<u64, String> {
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

/// Returns the bit width of a string literal segment, or `None` if the segment
/// is not a plain string literal (interpolations are not supported in binary segments).
pub(crate) fn string_segment_bit_width(seg: &BinarySegment) -> Option<u64> {
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

pub(crate) fn type_ann_bit_width(type_ann: &TypeExpr) -> Option<u64> {
    if let TypeExpr::Named { path, .. } = type_ann {
        let name = path.last()?;
        Primitive::from_name(name).and_then(|p| p.bit_width())
    } else {
        None
    }
}

pub(crate) fn is_float_segment(seg: &BinarySegment) -> bool {
    if let Some(TypeExpr::Named { path, .. }) = &seg.type_ann
        && let Some(name) = path.last()
    {
        return matches!(name.as_str(), "Float32" | "Float64");
    }
    false
}
