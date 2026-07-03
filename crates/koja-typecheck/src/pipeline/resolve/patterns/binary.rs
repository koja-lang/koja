//! Typecheck for `<<segments>>` binary patterns.
//!
//! Validates the subject is `Binary` / `Bits`, classifies each
//! segment (`LiteralInt` / `LiteralBytes` / `BindInt` / `Discard` /
//! `GreedyTail`), range-checks literal segments against their bit
//! width, and registers binding names into the arm's local scope so
//! the match-arm body can reference them.
//!
//! Pairs with [`super::super::literals::binary`]. They share the
//! "compute per-segment bit width" arithmetic but disagree on what
//! the segment's value represents (an expression to evaluate vs a
//! pattern element to bind / test against).
//!
//! Gated feature set (rejected with diagnostics):
//!
//! - Dynamic-width sizes (`::n` where `n` is not a literal int)
//! - `::N byte` unit
//! - Float-extract bindings (`<<x: Float32>>`)
//! - Bit-misaligned `: Binary` greedy tails (fixed prefix not
//!   byte-aligned)
//!
//! Bindings are registered with type:
//!
//! - Type-annotated (`x: Int8` / `x: UInt16` / `x: Binary` / `x: Bits`):
//!   the annotated primitive.
//! - Sized (`x::N` without annotation) and bare (`x`): `Int`
//!   (`Int64`).
//!
//! Sign-extension at extraction time is the LLVM emit phase's job.
//! Typecheck just propagates the `signed`/`unsigned` modifier into
//! the IR via the segment's `signedness` field.

use koja_ast::ast::{
    BinarySegment, BinarySignedness, BinaryUnit, Diagnostic, ExprKind, Literal, StringPart,
    TypeExpr, UnaryOp,
};
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::types::{display_resolution, is_primitive};
use super::PatternCoverage;

/// Resolve a `<<segments>>` pattern against `subject_ty`. Returns
/// [`PatternCoverage::Other`]. Binary patterns never satisfy the
/// catch-all rule, so the match driver requires a separate
/// wildcard arm for exhaustiveness (matching v1's behavior).
pub(super) fn resolve_binary_pattern(
    segments: &mut [BinarySegment],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    let is_binary_subject = is_primitive(subject_ty, resolver.registry, "Binary");
    let is_bits_subject = is_primitive(subject_ty, resolver.registry, "Bits");
    if subject_ty.is_resolved() && !(is_binary_subject || is_bits_subject) {
        diagnostics.push(Diagnostic::error(
            format!(
                "binary pattern requires `Binary` or `Bits` subject, got `{}`",
                display_resolution(subject_ty, resolver.registry),
            ),
            span,
        ));
        return PatternCoverage::Other;
    }

    let mut total_fixed_bits: u64 = 0;
    let mut has_greedy = false;
    let segment_count = segments.len();
    for (index, segment) in segments.iter_mut().enumerate() {
        let is_last = index == segment_count - 1;
        resolve_segment(
            segment,
            is_last,
            &mut total_fixed_bits,
            &mut has_greedy,
            resolver,
            diagnostics,
        );
    }
    PatternCoverage::Other
}

/// Per-segment dispatch on the `seg.value` shape: string literal /
/// integer literal / binding identifier / discard / greedy tail.
/// Width arithmetic and modifier validation are shared across all
/// branches via the helpers below.
fn resolve_segment(
    segment: &mut BinarySegment,
    is_last: bool,
    total_fixed_bits: &mut u64,
    has_greedy: &mut bool,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(byte_length) = string_segment_byte_length(segment) {
        if segment.size.is_some() || segment.type_ann.is_some() {
            diagnostics.push(Diagnostic::error(
                "typecheck: a `String`-valued binary pattern segment cannot \
                 carry a `::N` size or `:Type` annotation",
                segment.span,
            ));
            return;
        }
        *total_fixed_bits += byte_length.saturating_mul(8);
        return;
    }

    if check_greedy_tail(segment, is_last, *total_fixed_bits, resolver, diagnostics) {
        if *has_greedy {
            diagnostics.push(Diagnostic::error(
                "at most one greedy rest segment is allowed per binary pattern",
                segment.span,
            ));
        }
        *has_greedy = true;
        return;
    }

    if !is_last
        && segment.type_ann.is_some()
        && segment.size.is_none()
        && {
            let mut binding_segment = false;
            if let ExprKind::Ident { name, .. } = &segment.value.kind {
                binding_segment = name != "_";
            }
            binding_segment
        }
        && is_binary_or_bits_annotation(segment.type_ann.as_ref().unwrap())
    {
        diagnostics.push(Diagnostic::error(
            "greedy rest segment (`: Binary` / `: Bits`) must be the last segment of a \
             binary pattern",
            segment.span,
        ));
        return;
    }

    let Some(width_bits) = segment_fixed_width(segment, diagnostics) else {
        return;
    };

    check_orphan_modifiers(segment, diagnostics);

    match &segment.value.kind {
        ExprKind::Ident { name, .. } if name == "_" => {
            *total_fixed_bits += width_bits;
        }
        ExprKind::Ident { name, .. } => {
            let binding_ty = resolve_binding_type(segment, resolver, diagnostics);
            let local_id = resolver.scope.declare(name, binding_ty.clone());
            if let ExprKind::Ident { resolution, .. } = &mut segment.value.kind {
                *resolution = Resolution::Local(local_id);
            }
            segment.value.resolution = binding_ty;
            *total_fixed_bits += width_bits;
        }
        ExprKind::Literal { .. }
        | ExprKind::Unary {
            op: UnaryOp::Neg, ..
        } => {
            check_literal_overflow(
                &segment.value.kind,
                width_bits,
                segment.signedness,
                segment.span,
                diagnostics,
            );
            *total_fixed_bits += width_bits;
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "typecheck: binary pattern segment must be a literal, an \
                 identifier binding, `_`, or a string literal",
                segment.span,
            ));
        }
    }
}

/// Classify and validate a `: Binary` / `: Bits` greedy tail segment
/// (no explicit size, type annotation is the heap-payload family).
/// Registers the binding name on success. Returns `true` when the
/// segment is consumed here so the caller stops walking it.
fn check_greedy_tail(
    segment: &mut BinarySegment,
    is_last: bool,
    fixed_bits: u64,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if segment.size.is_some() {
        return false;
    }
    let Some(ann) = &segment.type_ann else {
        return false;
    };
    if !is_binary_or_bits_annotation(ann) {
        return false;
    }
    if !is_last {
        diagnostics.push(Diagnostic::error(
            "greedy rest segment (`: Binary` / `: Bits`) must be the last segment of a \
             binary pattern",
            segment.span,
        ));
        return true;
    }
    let TypeExpr::Named { path, .. } = ann else {
        return true;
    };
    let name = path.last().map(String::as_str).unwrap_or("");
    if name == "Binary" && !fixed_bits.is_multiple_of(8) {
        diagnostics.push(Diagnostic::error(
            format!("`: Binary` rest requires a byte-aligned prefix; got {fixed_bits} fixed bits",),
            segment.span,
        ));
        return true;
    }
    let binding_ty = resolver.registry.primitive(name);
    match &segment.value.kind {
        ExprKind::Ident { name, .. } if name != "_" => {
            let local_id = resolver.scope.declare(name, binding_ty.clone());
            if let ExprKind::Ident { resolution, .. } = &mut segment.value.kind {
                *resolution = Resolution::Local(local_id);
            }
            segment.value.resolution = binding_ty;
        }
        ExprKind::Ident { .. } => {
            segment.value.resolution = binding_ty;
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "greedy rest segment must be an identifier binding (or `_`)",
                segment.span,
            ));
        }
    }
    true
}

/// Compute a segment's fixed bit width (`::N` literal, primitive
/// type annotation, or the bare-segment default of 8). Diagnoses
/// dynamic sizes, byte-unit, and float-typed extracts.
fn segment_fixed_width(segment: &BinarySegment, diagnostics: &mut Vec<Diagnostic>) -> Option<u64> {
    if let Some(size_expr) = &segment.size {
        if segment.unit == BinaryUnit::Byte {
            diagnostics.push(Diagnostic::error(
                "typecheck: `::N byte` segment size is not supported in binary \
                 patterns (use `::M` bits instead)",
                segment.span,
            ));
            return None;
        }
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
                    "typecheck does not yet support dynamic-width binary pattern \
                     segments (`::n` where `n` is not a literal int)",
                    size_expr.span,
                ));
                return None;
            }
        };
        if bits == 0 {
            diagnostics.push(Diagnostic::error(
                "typecheck: a binary segment must carry at least 1 bit",
                segment.span,
            ));
            return None;
        }
        return Some(bits);
    }

    if let Some(type_ann) = &segment.type_ann {
        let TypeExpr::Named { path, .. } = type_ann else {
            diagnostics.push(Diagnostic::error(
                "typecheck: binary pattern segment type annotation must be a \
                 primitive name",
                segment.span,
            ));
            return None;
        };
        let name = path.last().map(String::as_str).unwrap_or("");
        return match name {
            "Int8" | "UInt8" => Some(8),
            "Int16" | "UInt16" => Some(16),
            "Int32" | "UInt32" => Some(32),
            "Int64" | "UInt64" => Some(64),
            "Float32" | "Float64" => {
                diagnostics.push(Diagnostic::error(
                    "typecheck does not yet support float-extract binary pattern \
                     segments (use a sized integer or an unsigned bit width)",
                    segment.span,
                ));
                None
            }
            other => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "typecheck: binary pattern segment type annotation \
                         `{other}` is not a recognized primitive (expected one of: \
                         Int8/16/32/64, UInt8/16/32/64)",
                    ),
                    segment.span,
                ));
                None
            }
        };
    }

    Some(8)
}

/// Sized bindings (`x::N`) and bare bindings type as `Int`
/// (i64), while type-annotated bindings (`x: Int8` / `x: UInt16` / …)
/// use the annotated primitive. The greedy-tail family (`x:
/// Binary` / `x: Bits`) is handled separately in
/// [`check_greedy_tail`].
fn resolve_binding_type(
    segment: &BinarySegment,
    resolver: &mut Resolver<'_>,
    _diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some(TypeExpr::Named { path, .. }) = &segment.type_ann {
        let name = path.last().map(String::as_str).unwrap_or("");
        return resolver.registry.primitive(name);
    }
    resolver.registry.primitive("Int")
}

/// Range-check an integer literal segment against its declared
/// bit width, honoring the `signed` / `unsigned` modifier. Ported
/// from v1's `check_literal_overflow`. Silently no-ops for widths
/// outside the 1..=64 supported range. `segment_fixed_width`
/// already rejects 0, and bit widths >64 are gated by other rules.
fn check_literal_overflow(
    kind: &ExprKind,
    bits: u64,
    signedness: Option<BinarySignedness>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if bits == 0 || bits > 64 {
        return;
    }
    let value = match kind {
        ExprKind::Literal {
            value: Literal::Int(n),
        } => n.parse::<i128>().ok(),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Literal {
                value: Literal::Int(n),
            } => n.parse::<i128>().ok().map(|v| -v),
            _ => None,
        },
        _ => None,
    };
    let Some(value) = value else {
        return;
    };
    let is_signed = signedness == Some(BinarySignedness::Signed);
    if is_signed {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        if value < min || value > max {
            diagnostics.push(Diagnostic::error(
                format!("{value} does not fit in {bits} signed bits (range {min}..{max})"),
                span,
            ));
        }
    } else {
        let max = if bits >= 128 {
            i128::MAX
        } else {
            (1i128 << bits) - 1
        };
        if value < 0 || value > max {
            diagnostics.push(Diagnostic::error(
                format!("{value} does not fit in {bits} unsigned bits (range 0..{max})"),
                span,
            ));
        }
    }
}

/// `signed` / `unsigned` / `big` / `little` modifiers require a
/// `::N` size or a primitive type annotation. Bare segments
/// don't carry enough shape for the modifier to mean anything.
/// V1 has the same rule.
fn check_orphan_modifiers(segment: &BinarySegment, diagnostics: &mut Vec<Diagnostic>) {
    if segment.signedness.is_some() && segment.size.is_none() && segment.type_ann.is_none() {
        diagnostics.push(Diagnostic::error(
            "signedness modifier requires a size specifier (`::N`)",
            segment.span,
        ));
    }
    if segment.endianness.is_some() && segment.size.is_none() && segment.type_ann.is_none() {
        diagnostics.push(Diagnostic::error(
            "endianness modifier requires a size specifier (`::N`)",
            segment.span,
        ));
    }
}

/// True when `ann` is a bare `Binary` / `Bits` primitive name,
/// the only two annotations that admit greedy tails.
fn is_binary_or_bits_annotation(ann: &TypeExpr) -> bool {
    let TypeExpr::Named { path, .. } = ann else {
        return false;
    };
    let name = path.last().map(String::as_str).unwrap_or("");
    matches!(name, "Binary" | "Bits")
}

/// Recover the byte length of a string-literal segment (no
/// interpolation). Returns `None` for non-string and interpolated
/// strings, since interpolation in binary patterns isn't supported.
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
