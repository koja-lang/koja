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
//!   `String`-typed values type as a string segment (no `::N`), the
//!   `Float32`/`Float64` type-annotation route forces a Float
//!   segment, `Binary`-typed values splice their bytes (bare or
//!   spelled `: Binary`), and everything else is an integer segment.
//! - Sum widths to a `total_bits`, then pick `Global.Binary` (when
//!   byte-aligned) or `Global.Bits` (otherwise) as the result type.
//!   Splices contribute a whole-byte count only known at runtime, so
//!   a literal containing one is always `Global.Binary` and every
//!   fixed-width run between splices must total whole bytes.
//!
//! Feature gaps surface as a diagnostic and leave the literal at
//! [`ResolvedType::unresolved`] so seal won't run. The segments are
//! still walked so any inner errors get reported in the same pass.

use koja_ast::ast::{
    BinarySegment, BinaryUnit, Diagnostic, ExprKind, Literal, StringPart, TypeExpr,
};
use koja_ast::identifier::ResolvedType;
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr;
use super::super::types::is_primitive;
use crate::registry::GlobalRegistry;

/// Per-segment kind decided from the AST modifiers and the
/// resolved value type. The IR lowering layer re-derives the same
/// shape from the AST during
/// [`koja_ir::lower::binary_literal`]. Typecheck only needs
/// to know that the value type is admissible and to count bits.
/// Carrying the kind alongside the bit width lets the typecheck
/// pass return one structured result per segment without committing
/// the IR vocabulary into the typecheck crate.
pub(crate) enum SegmentKind {
    Integer,
    Float,
    Splice,
    String,
}

/// Signedness of an integer binary segment: `UInt8`-style /
/// `::N` / bare segments are unsigned, `Int8`-style segments are
/// signed. Used to pick the literal range when overflow-checking
/// a constant-int segment value.
#[derive(Clone, Copy)]
enum IntSign {
    Signed,
    Unsigned,
}

/// Resolved per-segment metadata. `width_bits` is the segment's
/// total bit count (`byte_length * 8` for strings, the explicit
/// `::N` for sized integers, the type-annotation width for floats /
/// typed-integer forms, or `8` for an unmodified integer segment).
/// A `Binary` splice's byte count is only known at runtime, so it
/// carries `None`. `kind` lets the constant lift cross-check that a
/// segment's literal value agrees with its declared shape.
pub(crate) struct SegmentInfo {
    pub(crate) kind: SegmentKind,
    pub(crate) width_bits: Option<u64>,
}

impl SegmentInfo {
    fn fixed(kind: SegmentKind, width_bits: u64) -> Self {
        Self {
            kind,
            width_bits: Some(width_bits),
        }
    }

    fn splice() -> Self {
        Self {
            kind: SegmentKind::Splice,
            width_bits: None,
        }
    }
}

/// Resolve a `<<segments>>` literal. Walks each segment (recursing
/// into `seg.value` so any inner ill-typedness still surfaces),
/// computes a total bit width, and returns `Global.Binary` (when
/// byte-aligned) / `Global.Bits` (otherwise) as the literal's type.
/// A literal containing splices is always `Global.Binary`, and each
/// fixed-width run between splices must total whole bytes (the
/// lowerer builds each run as its own `Binary` and concatenates).
/// Returns `ResolvedType::unresolved()` when any per-segment check
/// errors so the seal pass declines to descend.
pub(in super::super) fn resolve_binary_literal(
    segments: &mut [BinarySegment],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let mut total_bits: u64 = 0;
    let mut run_bits: u64 = 0;
    let mut has_splice = false;
    let mut all_resolved = true;
    for segment in segments.iter_mut() {
        resolve_expr(&mut segment.value, resolver, diagnostics);
        match resolve_segment(segment, resolver.registry, diagnostics) {
            Some(SegmentInfo {
                width_bits: Some(bits),
                ..
            }) => {
                total_bits += bits;
                run_bits += bits;
            }
            Some(SegmentInfo {
                width_bits: None, ..
            }) => {
                has_splice = true;
                if !run_bits.is_multiple_of(8) {
                    diagnostics.push(unaligned_run_diagnostic(segment.span));
                    all_resolved = false;
                }
                run_bits = 0;
            }
            None => all_resolved = false,
        }
    }
    if has_splice && !run_bits.is_multiple_of(8) {
        diagnostics.push(unaligned_run_diagnostic(span));
        all_resolved = false;
    }
    if !all_resolved {
        return ResolvedType::unresolved();
    }
    let primitive_name = if has_splice || total_bits.is_multiple_of(8) {
        "Binary"
    } else {
        "Bits"
    };
    resolver.registry.primitive(primitive_name)
}

fn unaligned_run_diagnostic(span: Span) -> Diagnostic {
    Diagnostic::error(
        "typecheck: the fixed-width segments around a `Binary` splice \
         must total whole bytes",
        span,
    )
}

/// Validate one segment and produce its [`SegmentInfo`] (`None` plus
/// a diagnostic on failure). Surfaces feature-gap diagnostics for
/// the v1 dynamic-width form (`::n` where `n` is a runtime int).
/// Also called by the constant lift, which stamps segment value
/// resolutions itself before delegating the width and fit rules here.
pub(crate) fn resolve_segment(
    segment: &BinarySegment,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SegmentInfo> {
    if let Some(byte_length) = string_segment_byte_length(segment) {
        if segment.size.is_some() || segment.type_ann.is_some() {
            diagnostics.push(Diagnostic::error(
                "typecheck: a `String`-valued binary segment cannot \
                 carry a `::N` size or `:Type` annotation",
                segment.span,
            ));
            return None;
        }
        return Some(SegmentInfo::fixed(SegmentKind::String, byte_length * 8));
    }

    if let Some(size_expr) = &segment.size {
        let bits = match &size_expr.kind {
            ExprKind::Literal {
                value: Literal::Int(n),
            } => match n.parse::<u64>() {
                Ok(parsed) => parsed,
                Err(_) => {
                    diagnostics.push(Diagnostic::error(
                        format!("typecheck: invalid binary segment size literal `{n}`"),
                        size_expr.span,
                    ));
                    return None;
                }
            },
            _ => {
                diagnostics.push(Diagnostic::error(
                    "typecheck does not yet support dynamic-width binary segments \
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
                "typecheck: a binary segment must carry at least 1 bit",
                segment.span,
            ));
            return None;
        }
        // `::N` only validly applies to integer-typed values today.
        // Float segments use `: Float32` / `: Float64`, and string
        // segments don't carry a size. Reject loud mismatches so a
        // misuse like `1.0 :: 16` doesn't silently coerce.
        if !is_primitive(&segment.value.resolution, registry, "Int") {
            diagnostics.push(Diagnostic::error(
                "typecheck: `::N` segment size requires an `Int`-typed value",
                segment.span,
            ));
            return None;
        }
        if !literal_fits_int_segment(segment, width_bits, IntSign::Unsigned, diagnostics) {
            return None;
        }
        return Some(SegmentInfo::fixed(SegmentKind::Integer, width_bits));
    }

    if let Some(type_ann) = &segment.type_ann {
        let TypeExpr::Named { path, .. } = type_ann else {
            diagnostics.push(Diagnostic::error(
                "typecheck: binary segment type annotation must be a primitive name",
                segment.span,
            ));
            return None;
        };
        let name = path.last().map(String::as_str).unwrap_or("");
        let (kind, width_bits, sign) = match name {
            "Binary" => return splice_segment_info(segment, registry, diagnostics),
            "Float32" => (SegmentKind::Float, 32u64, None),
            "Float64" => (SegmentKind::Float, 64u64, None),
            "Int8" => (SegmentKind::Integer, 8u64, Some(IntSign::Signed)),
            "UInt8" => (SegmentKind::Integer, 8u64, Some(IntSign::Unsigned)),
            "Int16" => (SegmentKind::Integer, 16u64, Some(IntSign::Signed)),
            "UInt16" => (SegmentKind::Integer, 16u64, Some(IntSign::Unsigned)),
            "Int32" => (SegmentKind::Integer, 32u64, Some(IntSign::Signed)),
            "UInt32" => (SegmentKind::Integer, 32u64, Some(IntSign::Unsigned)),
            "Int64" => (SegmentKind::Integer, 64u64, Some(IntSign::Signed)),
            "UInt64" => (SegmentKind::Integer, 64u64, Some(IntSign::Unsigned)),
            "Bits" | "String" => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "typecheck: `{name}` values cannot be spliced into a binary \
                         construction (only `Binary` splices are supported, convert \
                         with `.to_binary()` first)",
                    ),
                    segment.span,
                ));
                return None;
            }
            other => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "typecheck: binary segment type annotation `{other}` is not \
                         a recognized primitive (expected one of: Int8/16/32/64, \
                         UInt8/16/32/64, Float32, Float64, Binary)",
                    ),
                    segment.span,
                ));
                return None;
            }
        };
        if let Some(sign) = sign
            && !literal_fits_int_segment(segment, width_bits, sign, diagnostics)
        {
            return None;
        }
        return Some(SegmentInfo::fixed(kind, width_bits));
    }

    // A bare segment classifies by the value's type. `Int` takes the
    // default 8-bit unsigned width and `Binary` splices its bytes.
    if is_primitive(&segment.value.resolution, registry, "Binary") {
        return splice_segment_info(segment, registry, diagnostics);
    }
    if !is_primitive(&segment.value.resolution, registry, "Int") {
        diagnostics.push(Diagnostic::error(
            "typecheck: bare binary segment requires an `Int`-typed value \
             (packed as 8 unsigned bits) or a `Binary`-typed value (spliced \
             whole); use a `: Type` / `::N` modifier to spell out any other \
             segment shape",
            segment.span,
        ));
        return None;
    }
    if !literal_fits_int_segment(segment, 8, IntSign::Unsigned, diagnostics) {
        return None;
    }
    Some(SegmentInfo::fixed(SegmentKind::Integer, 8))
}

/// Validate a `Binary` splice segment (bare `payload` or explicit
/// `payload: Binary`). Splices copy whole bytes as-is, so width,
/// signedness, and endianness modifiers have nothing to apply to.
fn splice_segment_info(
    segment: &BinarySegment,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SegmentInfo> {
    if !is_primitive(&segment.value.resolution, registry, "Binary") {
        diagnostics.push(Diagnostic::error(
            "typecheck: a `: Binary` splice segment requires a `Binary`-typed value",
            segment.span,
        ));
        return None;
    }
    if segment.signedness.is_some() || segment.endianness.is_some() {
        diagnostics.push(Diagnostic::error(
            "typecheck: a `Binary` splice segment cannot carry `signed` / \
             `unsigned` or endianness modifiers",
            segment.span,
        ));
        return None;
    }
    Some(SegmentInfo::splice())
}

/// Validate that a constant-int segment value fits the segment's
/// declared bit width and signedness. Non-literal values pass
/// (the IR lowering enforces width at runtime). Emits a "does not
/// fit in N {un}signed bits" diagnostic on overflow.
fn literal_fits_int_segment(
    segment: &BinarySegment,
    width_bits: u64,
    sign: IntSign,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let ExprKind::Literal {
        value: Literal::Int(text),
    } = &segment.value.kind
    else {
        return true;
    };
    let Ok(value) = text.parse::<i128>() else {
        return true;
    };
    if width_bits == 0 || width_bits > 127 {
        return true;
    }
    let (low, high) = match sign {
        IntSign::Signed => {
            let half = 1i128 << (width_bits - 1);
            (-half, half - 1)
        }
        IntSign::Unsigned => (0, (1i128 << width_bits) - 1),
    };
    if value >= low && value <= high {
        return true;
    }
    let sign_label = match sign {
        IntSign::Signed => "signed",
        IntSign::Unsigned => "unsigned",
    };
    diagnostics.push(Diagnostic::error(
        format!("value `{value}` does not fit in {width_bits} {sign_label} bits"),
        segment.value.span,
    ));
    false
}

/// Recover the byte length of a string-literal segment (no
/// interpolations). Returns `None` for non-string and interpolated
/// strings, since interpolation in binary segments is a feature gap
/// (and the same is true everywhere else interpolation appears).
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
