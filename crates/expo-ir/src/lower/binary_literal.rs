//! Lowering for `<<segments>>` binary literals to
//! [`IRInstruction::BinaryConstruct`].
//!
//! Per-segment, the typecheck layer has already validated value
//! type vs modifier — here we re-derive the per-segment width and
//! kind (cheaper to recompute than to thread an annotation through
//! the AST) plus the running `bit_offset` accumulator. The result
//! is a [`ResolvedBinaryLayout`] + a `Vec<LoweredBinarySegment>`
//! that the LLVM backend and eval interpreter both consume.
//!
//! Pairs with [`expo_typecheck::pipeline::resolve::binary_literal`]
//! — the two layers have to agree on width arithmetic, but the
//! typecheck side enforces type correctness while the lower side
//! just stamps offsets.

use expo_ast::ast::{
    BinaryEndianness, BinarySegment, BinarySignedness, BinaryUnit, Diagnostic, Expr, ExprKind,
    Literal, StringPart, TypeExpr,
};
use expo_ast::span::Span;
use expo_typecheck::GlobalRegistry;

use crate::function::{IRBlockId, IRInstruction};
use crate::types::{
    BinaryEndian, BinarySign, IRType, LoweredBinarySegment, ResolvedBinaryLayout, ValueId,
};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;

/// Lower a `<<segments>>` literal: lower each segment's `value`
/// expression, decide its width / kind / endianness, and assemble
/// the resulting [`IRInstruction::BinaryConstruct`].
pub(super) fn lower_binary_literal(
    segments: &[BinarySegment],
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let mut current_block = block;
    let mut lowered: Vec<LoweredBinarySegment> = Vec::with_capacity(segments.len());
    let mut bit_offset: u64 = 0;

    for segment in segments {
        let kind = classify_segment(segment, span, &mut output.diagnostics)?;
        let (value, next_block) = lower_expr(&segment.value, ctx, current_block, registry, output)?;
        current_block = next_block;
        let lowered_segment = match kind {
            ClassifiedSegment::Integer { width } => LoweredBinarySegment::Integer {
                value,
                width,
                sign: ast_signedness_to_ir(segment.signedness),
                endian: ast_endianness_to_ir(segment.endianness),
                bit_offset,
            },
            ClassifiedSegment::Float { width } => LoweredBinarySegment::Float {
                value,
                width,
                endian: ast_endianness_to_ir(segment.endianness),
                bit_offset,
            },
            ClassifiedSegment::String { byte_length } => LoweredBinarySegment::String {
                value,
                byte_length,
                bit_offset,
            },
        };
        bit_offset += lowered_segment.width();
        lowered.push(lowered_segment);
    }

    let layout = ResolvedBinaryLayout {
        total_bits: bit_offset,
        byte_aligned: bit_offset.is_multiple_of(8),
    };
    let dest_ty = if layout.byte_aligned {
        IRType::Binary
    } else {
        IRType::Bits
    };
    let dest = ctx.fresh_value(dest_ty);
    ctx.cfg.append(
        current_block,
        IRInstruction::BinaryConstruct {
            dest,
            layout,
            segments: lowered,
        },
    );
    Ok((dest, current_block))
}

enum ClassifiedSegment {
    Integer { width: u64 },
    Float { width: u64 },
    String { byte_length: u64 },
}

/// Per-segment kind + width derivation. Mirrors
/// [`expo_typecheck::pipeline::resolve::binary_literal::resolve_segment`]
/// — both call sites need the same width arithmetic to agree, so
/// the rules live in two places (typecheck side rejects bad
/// shapes; lower side assumes typecheck has already passed and
/// just transcribes). On a typecheck/lower mismatch we still
/// surface a diagnostic rather than panicking, so a regression
/// surfaces as a build-time error instead of a runtime crash.
fn classify_segment(
    segment: &BinarySegment,
    literal_span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ClassifiedSegment, ()> {
    if let Some(byte_length) = string_segment_byte_length(&segment.value) {
        return Ok(ClassifiedSegment::String { byte_length });
    }

    if let Some(size_expr) = &segment.size {
        let bits = match &size_expr.kind {
            ExprKind::Literal {
                value: Literal::Int(n),
            } => match n.parse::<u64>() {
                Ok(parsed) => parsed,
                Err(_) => {
                    diagnostics.push(Diagnostic::error(
                        format!("IR lower: invalid binary segment size literal `{n}`"),
                        size_expr.span,
                    ));
                    return Err(());
                }
            },
            _ => {
                diagnostics.push(Diagnostic::error(
                    "IR lower: dynamic-width binary segments are not yet supported",
                    size_expr.span,
                ));
                return Err(());
            }
        };
        let width = match segment.unit {
            BinaryUnit::Bit => bits,
            BinaryUnit::Byte => bits.saturating_mul(8),
        };
        return Ok(ClassifiedSegment::Integer { width });
    }

    if let Some(type_ann) = &segment.type_ann {
        if let TypeExpr::Named { path, .. } = type_ann {
            let name = path.last().map(String::as_str).unwrap_or("");
            return Ok(match name {
                "Float32" => ClassifiedSegment::Float { width: 32 },
                "Float64" => ClassifiedSegment::Float { width: 64 },
                "Int8" | "UInt8" => ClassifiedSegment::Integer { width: 8 },
                "Int16" | "UInt16" => ClassifiedSegment::Integer { width: 16 },
                "Int32" | "UInt32" => ClassifiedSegment::Integer { width: 32 },
                "Int64" | "UInt64" => ClassifiedSegment::Integer { width: 64 },
                other => {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "IR lower: unrecognized binary segment type annotation `{other}` \
                             — typecheck must have allowed an unsupported width",
                        ),
                        literal_span,
                    ));
                    return Err(());
                }
            });
        }
        diagnostics.push(Diagnostic::error(
            "IR lower: binary segment type annotation must be a primitive name",
            literal_span,
        ));
        return Err(());
    }

    Ok(ClassifiedSegment::Integer { width: 8 })
}

fn string_segment_byte_length(value: &Expr) -> Option<u64> {
    let ExprKind::String { parts, .. } = &value.kind else {
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
