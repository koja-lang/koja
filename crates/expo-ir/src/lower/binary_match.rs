//! Lowering for `<<segments>>` binary patterns to
//! [`IRInstruction::BinaryMatch`].
//!
//! Mirrors the segment-classification logic of
//! [`super::binary_literal`] — typecheck has already validated
//! the segment shape (sized integer / typed integer / string
//! literal / binding / discard / greedy tail), so the lower pass
//! just transcribes each segment to its
//! [`LoweredBinaryPattern`] form while tracking a running
//! `bit_offset` accumulator.
//!
//! Binding segments mint their `LocalDecl`s in the function's
//! entry block (idempotent per local) so the seal pass's
//! "every-`LocalWrite` is dominated by a `LocalDecl`" rule still
//! holds when the LLVM emit phase stamps the extracted value
//! into the slot.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{
    BinaryEndianness, BinarySegment, BinarySignedness, ExprKind, Literal, StringPart, TypeExpr,
    UnaryOp,
};
use expo_ast::identifier::Resolution;

use crate::function::{IRBlockId, IRInstruction};
use crate::local::IRLocalId;
use crate::types::{
    BinaryEndian, BinarySign, IRType, LoweredBinaryMatchLayout, LoweredBinaryPattern, ValueId,
};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::patterns::ensure_local_declared;

/// Lower a `Pattern::Binary` against `subject` and append an
/// `IRInstruction::BinaryMatch` to `block`. Returns the `i1`
/// `ValueId` the match driver wires into the arm's `CondBranch`.
/// Bindings inside the pattern are registered via `LocalDecl` in
/// the function's entry block; the actual value writes happen as
/// a side effect of the `BinaryMatch` instruction at emit time.
pub(super) fn lower_binary_pattern(
    segments: &[BinarySegment],
    subject: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> ValueId {
    let mut lowered: Vec<LoweredBinaryPattern> = Vec::with_capacity(segments.len());
    let mut bit_offset: u64 = 0;
    let mut has_greedy_tail = false;

    for segment in segments {
        let Some(lowered_segment) = lower_segment(segment, bit_offset, ctx, registry, output)
        else {
            continue;
        };
        match &lowered_segment {
            LoweredBinaryPattern::GreedyTail { .. } => has_greedy_tail = true,
            LoweredBinaryPattern::LiteralInt { width, .. }
            | LoweredBinaryPattern::BindInt { width, .. }
            | LoweredBinaryPattern::Discard { width, .. } => bit_offset += *width,
            LoweredBinaryPattern::LiteralBytes { bytes, .. } => {
                bit_offset += (bytes.len() as u64).saturating_mul(8);
            }
        }
        lowered.push(lowered_segment);
    }

    let layout = LoweredBinaryMatchLayout {
        fixed_bits: bit_offset,
        has_greedy_tail,
    };
    let dest = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryMatch {
            dest,
            layout,
            segments: lowered,
            subject,
        },
    );
    dest
}

/// Per-segment classification: figure out the bit width from
/// `::N` / `: Type` / bare default, then dispatch on
/// `seg.value.kind`. Mirrors the typecheck-side classifier so the
/// two layers see the same shape — lower never diagnoses (errors
/// are typecheck-only) and falls back to a discard when an
/// unsupported shape leaks through.
fn lower_segment(
    segment: &BinarySegment,
    bit_offset: u64,
    ctx: &mut FnLowerCtx,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<LoweredBinaryPattern> {
    if let Some((bytes, byte_length)) = string_segment_bytes(segment) {
        let _ = byte_length;
        return Some(LoweredBinaryPattern::LiteralBytes { bit_offset, bytes });
    }

    if let Some(tail) = lower_greedy_tail(segment, bit_offset, ctx, registry, output) {
        return Some(tail);
    }

    let width = segment_fixed_width(segment)?;
    let endian = ast_endianness_to_ir(segment.endianness);
    let sign = ast_signedness_to_ir(segment.signedness);

    match &segment.value.kind {
        ExprKind::Ident { name, .. } if name == "_" => {
            Some(LoweredBinaryPattern::Discard { bit_offset, width })
        }
        ExprKind::Ident { resolution, name } => {
            let Resolution::Local(local_id) = resolution else {
                panic!(
                    "alpha IR lower: binary pattern binding `{name}` carries no \
                     Resolution::Local — resolver invariant violation",
                );
            };
            let local = IRLocalId::from_local_id(*local_id);
            let ty = super::package::resolved_type_to_ir_type(
                &segment.value.resolution,
                registry,
                &mut output.instantiations,
            );
            ensure_local_declared(local, &ty, ctx);
            Some(LoweredBinaryPattern::BindInt {
                bit_offset,
                endian,
                local,
                sign,
                ty,
                width,
            })
        }
        ExprKind::Literal {
            value: Literal::Int(n),
        } => super::ops::parse_int_literal(n)
            .ok()
            .map(|parsed| LoweredBinaryPattern::LiteralInt {
                bit_offset,
                endian,
                sign,
                value: parsed as i128,
                width,
            }),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &operand.kind {
            ExprKind::Literal {
                value: Literal::Int(n),
            } => super::ops::parse_int_literal(n).ok().map(|parsed| {
                LoweredBinaryPattern::LiteralInt {
                    bit_offset,
                    endian,
                    sign,
                    value: -(parsed as i128),
                    width,
                }
            }),
            _ => None,
        },
        _ => None,
    }
}

/// A greedy tail is a binding-or-discard segment whose only
/// annotation is `: Binary` or `: Bits`. Typecheck has already
/// validated it is the last segment and (for `: Binary`) the
/// fixed prefix is byte-aligned.
fn lower_greedy_tail(
    segment: &BinarySegment,
    bit_offset: u64,
    ctx: &mut FnLowerCtx,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<LoweredBinaryPattern> {
    if segment.size.is_some() {
        return None;
    }
    let TypeExpr::Named { path, .. } = segment.type_ann.as_ref()? else {
        return None;
    };
    let name = path.last().map(String::as_str).unwrap_or("");
    let ty = match name {
        "Binary" => IRType::Binary,
        "Bits" => IRType::Bits,
        _ => return None,
    };
    let local = match &segment.value.kind {
        ExprKind::Ident {
            resolution: Resolution::Local(id),
            ..
        } => {
            let l = IRLocalId::from_local_id(*id);
            ensure_local_declared(l, &ty, ctx);
            Some(l)
        }
        ExprKind::Ident { name, .. } if name == "_" => None,
        _ => return None,
    };
    let _ = registry;
    let _ = output;
    Some(LoweredBinaryPattern::GreedyTail {
        bit_offset,
        local,
        ty,
    })
}

/// Per-segment fixed bit width. `None` only on a typecheck-bypassed
/// shape (we already diagnosed dynamic widths / byte-unit /
/// unsupported types) — the catch-all returns `None` rather than
/// panicking so a stale typecheck diagnostic doesn't trip a lower
/// crash.
fn segment_fixed_width(segment: &BinarySegment) -> Option<u64> {
    if let Some(size_expr) = &segment.size {
        let ExprKind::Literal {
            value: Literal::Int(n),
        } = &size_expr.kind
        else {
            return None;
        };
        return super::ops::parse_int_literal(n).ok().map(|v| v as u64);
    }
    if let Some(TypeExpr::Named { path, .. }) = &segment.type_ann {
        let name = path.last().map(String::as_str).unwrap_or("");
        return match name {
            "Int8" | "UInt8" => Some(8),
            "Int16" | "UInt16" => Some(16),
            "Int32" | "UInt32" => Some(32),
            "Int64" | "UInt64" => Some(64),
            _ => None,
        };
    }
    Some(8)
}

/// Read out the literal bytes of a string-segment value. Returns
/// `None` for non-string and for interpolated strings (typecheck
/// already rejects the latter — this is belt-and-suspenders).
fn string_segment_bytes(segment: &BinarySegment) -> Option<(Vec<u8>, u64)> {
    let ExprKind::String { parts, .. } = &segment.value.kind else {
        return None;
    };
    let mut bytes: Vec<u8> = Vec::new();
    for part in parts {
        match part {
            StringPart::Literal { value, .. } => bytes.extend_from_slice(value.as_bytes()),
            StringPart::Interpolation { .. } => return None,
        }
    }
    let byte_length = bytes.len() as u64;
    Some((bytes, byte_length))
}

fn ast_endianness_to_ir(endian: Option<BinaryEndianness>) -> BinaryEndian {
    match endian.unwrap_or(BinaryEndianness::Big) {
        BinaryEndianness::Big => BinaryEndian::Big,
        BinaryEndianness::Little => BinaryEndian::Little,
    }
}

fn ast_signedness_to_ir(sign: Option<BinarySignedness>) -> BinarySign {
    match sign.unwrap_or(BinarySignedness::Unsigned) {
        BinarySignedness::Signed => BinarySign::Signed,
        BinarySignedness::Unsigned => BinarySign::Unsigned,
    }
}
