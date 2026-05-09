//! Typecheck for `<<segments>>` binary literals.
//!
//! Per-segment validation:
//!
//! - Recurse on `seg.value` so its type is resolved.
//! - Compute the segment's bit width from one of: `::N` (literal int
//!   size, optionally `byte`-unit-multiplied), `: TypeAnn` (named
//!   primitive width: `Int8`/`UInt16`/.../`Float32`/`Float64`), or
//!   the bare-segment default of 8 bits when neither is given.
//! - Validate the value type against the segment's modifier:
//!   `String`-typed values type as a string segment (no `::N`); the
//!   `Float32`/`Float64` type-annotation route forces a Float
//!   segment; everything else is an integer segment.
//! - Sum widths to a `total_bits`; pick `Global.Binary` (when
//!   byte-aligned) or `Global.Bits` (otherwise) as the result type.
//!
//! Feature gaps surface as a diagnostic and leave the literal at
//! [`ResolvedType::unresolved`] so seal won't run; the segments are
//! still walked so any inner errors get reported in the same pass.

use expo_ast::ast::{
    BinarySegment, BinaryUnit, Diagnostic, ExprKind, Literal, StringPart, TypeExpr,
};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::is_primitive;
use crate::registry::GlobalRegistry;

/// Per-segment kind decided from the AST modifiers and the
/// resolved value type. The IR lowering layer re-derives the same
/// shape from the AST during
/// [`expo_alpha_ir::lower::binary_literal`] — typecheck only needs
/// to know that the value type is admissible and to count bits.
/// Carrying the kind alongside the bit width lets the typecheck
/// pass return one structured result per segment without committing
/// the IR vocabulary into the typecheck crate.
enum SegmentKind {
    Integer,
    Float,
    String,
}

/// Resolved per-segment metadata. `width_bits` is the segment's
/// total bit count (`byte_length * 8` for strings, the explicit
/// `::N` for sized integers, the type-annotation width for floats /
/// typed-integer forms, or `8` for an unmodified integer segment).
/// `kind` is kept for future binary-pattern reuse — pattern
/// resolution will need to know the kind to decide what shape of
/// bound variable to mint, and the typecheck-side classifier is
/// the single canonical place that decides.
struct SegmentInfo {
    #[allow(dead_code)]
    kind: SegmentKind,
    width_bits: u64,
}

/// Resolve a `<<segments>>` literal. Walks each segment (recursing
/// into `seg.value` so any inner ill-typedness still surfaces),
/// computes a total bit width, and returns `Global.Binary` (when
/// byte-aligned) / `Global.Bits` (otherwise) as the literal's type.
/// Returns `ResolvedType::unresolved()` when any per-segment check
/// errors so the seal pass declines to descend.
pub(super) fn resolve_binary_literal(
    segments: &mut [BinarySegment],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let mut total_bits: u64 = 0;
    let mut all_resolved = true;
    for segment in segments.iter_mut() {
        resolve_expr(&mut segment.value, resolver, diagnostics);
        match resolve_segment(segment, resolver.registry, diagnostics) {
            Some(info) => total_bits += info.width_bits,
            None => all_resolved = false,
        }
    }
    if !all_resolved {
        return ResolvedType::unresolved();
    }
    let _ = span;
    let primitive_name = if total_bits.is_multiple_of(8) {
        "Binary"
    } else {
        "Bits"
    };
    resolver.registry.primitive(primitive_name)
}

/// Validate one segment and produce its [`SegmentInfo`] (`None` plus
/// a diagnostic on failure). Surfaces feature-gap diagnostics for
/// the v1 dynamic-width form (`::n` where `n` is a runtime int).
fn resolve_segment(
    segment: &BinarySegment,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SegmentInfo> {
    if let Some(byte_length) = string_segment_byte_length(segment) {
        if segment.size.is_some() || segment.type_ann.is_some() {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck: a `String`-valued binary segment cannot \
                 carry a `::N` size or `:Type` annotation",
                segment.span,
            ));
            return None;
        }
        return Some(SegmentInfo {
            kind: SegmentKind::String,
            width_bits: byte_length * 8,
        });
    }

    if let Some(size_expr) = &segment.size {
        let bits = match &size_expr.kind {
            ExprKind::Literal {
                value: Literal::Int(n),
            } => match n.parse::<u64>() {
                Ok(parsed) => parsed,
                Err(_) => {
                    diagnostics.push(Diagnostic::error(
                        format!("alpha typecheck: invalid binary segment size literal `{n}`"),
                        size_expr.span,
                    ));
                    return None;
                }
            },
            _ => {
                diagnostics.push(Diagnostic::error(
                    "alpha typecheck does not yet support dynamic-width binary segments \
                     (`::n` where `n` is not a literal int)",
                    size_expr.span,
                ));
                return None;
            }
        };
        let width_bits = match segment.unit {
            BinaryUnit::Bit => bits,
            BinaryUnit::Byte => bits.saturating_mul(8),
        };
        if width_bits == 0 {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck: a binary segment must carry at least 1 bit",
                segment.span,
            ));
            return None;
        }
        // `::N` only validly applies to integer-typed values today.
        // Float segments use `: Float32` / `: Float64`; string
        // segments don't carry a size. Reject loud mismatches so a
        // misuse like `1.0 :: 16` doesn't silently coerce.
        if !is_primitive(&segment.value.resolution, registry, "Int") {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck: `::N` segment size requires an `Int`-typed value",
                segment.span,
            ));
            return None;
        }
        return Some(SegmentInfo {
            kind: SegmentKind::Integer,
            width_bits,
        });
    }

    if let Some(type_ann) = &segment.type_ann {
        let TypeExpr::Named { path, .. } = type_ann else {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck: binary segment type annotation must be a primitive name",
                segment.span,
            ));
            return None;
        };
        let name = path.last().map(String::as_str).unwrap_or("");
        let (kind, width_bits) = match name {
            "Float32" => (SegmentKind::Float, 32u64),
            "Float64" => (SegmentKind::Float, 64u64),
            "Int8" | "UInt8" => (SegmentKind::Integer, 8u64),
            "Int16" | "UInt16" => (SegmentKind::Integer, 16u64),
            "Int32" | "UInt32" => (SegmentKind::Integer, 32u64),
            "Int64" | "UInt64" => (SegmentKind::Integer, 64u64),
            other => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck: binary segment type annotation `{other}` is not \
                         a recognized primitive (expected one of: Int8/16/32/64, \
                         UInt8/16/32/64, Float32, Float64)",
                    ),
                    segment.span,
                ));
                return None;
            }
        };
        return Some(SegmentInfo { kind, width_bits });
    }

    // Bare segment: integer value, default 8-bit width.
    if !is_primitive(&segment.value.resolution, registry, "Int") {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck: bare binary segment requires an `Int`-typed value (or use \
             a `: Type` / `::N` modifier to spell out the segment shape)",
            segment.span,
        ));
        return None;
    }
    Some(SegmentInfo {
        kind: SegmentKind::Integer,
        width_bits: 8,
    })
}

/// Recover the byte length of a string-literal segment (no
/// interpolations). Returns `None` for non-string and interpolated
/// strings — interpolation in binary segments isn't supported in
/// alpha (interpolation is a feature gap everywhere else too).
fn string_segment_byte_length(segment: &BinarySegment) -> Option<u64> {
    let ExprKind::String { parts, .. } = &segment.value.kind else {
        return None;
    };
    let mut byte_length: u64 = 0;
    for part in parts {
        match part {
            StringPart::Literal { value, .. } => byte_length += value.len() as u64,
            StringPart::Interpolation { .. } => return None,
        }
    }
    Some(byte_length)
}
