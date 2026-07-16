//! Lowering for `<<segments>>` binary literals to
//! [`IRInstruction::BinaryConstruct`].
//!
//! Per-segment, the typecheck layer has already validated value
//! type vs modifier. Here we re-derive the per-segment width and
//! kind (cheaper to recompute than to thread an annotation through
//! the AST) plus the running `bit_offset` accumulator. The result
//! is a [`ResolvedBinaryLayout`] + a `Vec<LoweredBinarySegment>`
//! that the LLVM backend and eval interpreter both consume.
//!
//! `Binary` splice segments desugar here. Fixed-width segments
//! between splices form static runs, each run becomes its own
//! `BinaryConstruct`, and the runs and spliced values fold into a
//! chain of [`IRInstruction::Concat`]s. Neither backend knows
//! splices exist.
//!
//! Pairs with [`koja_typecheck::pipeline::resolve::binary_literal`].
//! The two layers have to agree on width arithmetic, but the
//! typecheck side enforces type correctness while the lower side
//! just stamps offsets.

use koja_ast::ast::{
    BinaryEndianness, BinarySegment, BinarySignedness, BinaryUnit, Diagnostic, Expr, ExprKind,
    Literal, StringPart, TypeExpr,
};
use koja_ast::span::Span;
use koja_typecheck::GlobalRegistry;

use crate::function::{IRBlockId, IRInstruction};
use crate::types::{
    BinaryEndian, BinarySign, ConcatKind, IRType, LoweredBinarySegment, ResolvedBinaryLayout,
    ValueId,
};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::ownership::{drop_discarded_temp, materialize_owned};

/// Lower a `<<segments>>` literal: lower each segment's `value`
/// expression, decide its width / kind / endianness, and assemble
/// the resulting [`IRInstruction::BinaryConstruct`]. Splice
/// segments split the literal into concat operands that
/// [`concat_operands`] folds back together.
pub(super) fn lower_binary_literal(
    segments: &[BinarySegment],
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let mut current_block = block;
    let mut builder = ConcatBuilder::default();

    for segment in segments {
        let (value, next_block) = lower_expr(&segment.value, ctx, current_block, registry, output)?;
        current_block = next_block;
        // Only bare segments classify by value type (Binary means
        // splice), so skip the lookup when a modifier decides.
        let bare = segment.size.is_none() && segment.type_ann.is_none();
        let value_ty = if bare { Some(ctx.type_of(value)) } else { None };
        let kind = classify_segment(segment, span, value_ty.as_ref(), &mut output.diagnostics)?;
        let lowered_segment = match kind {
            ClassifiedSegment::Integer { width } => LoweredBinarySegment::Integer {
                value,
                width,
                sign: ast_signedness_to_ir(segment.signedness),
                endian: ast_endianness_to_ir(segment.endianness),
                bit_offset: builder.run_bits,
            },
            ClassifiedSegment::Float { width } => LoweredBinarySegment::Float {
                value,
                width,
                endian: ast_endianness_to_ir(segment.endianness),
                bit_offset: builder.run_bits,
            },
            ClassifiedSegment::String { byte_length } => LoweredBinarySegment::String {
                value,
                byte_length,
                bit_offset: builder.run_bits,
            },
            ClassifiedSegment::Splice => {
                builder.has_splice = true;
                builder.flush(span, ctx, current_block, output)?;
                builder.operands.push(value);
                continue;
            }
        };
        builder.push(lowered_segment);
    }

    builder.flush(span, ctx, current_block, output)?;
    let dest = concat_operands(builder.operands, ctx, current_block);
    Ok((dest, current_block))
}

/// Walk state for one literal: the pending fixed-width run and the
/// finished concat operands (run constructs and spliced values) in
/// source order.
#[derive(Default)]
struct ConcatBuilder {
    has_splice: bool,
    operands: Vec<ValueId>,
    run: Vec<LoweredBinarySegment>,
    run_bits: u64,
}

impl ConcatBuilder {
    fn push(&mut self, segment: LoweredBinarySegment) {
        self.run_bits += segment.width();
        self.run.push(segment);
    }

    /// Emit the pending run (if any) as its own `BinaryConstruct`
    /// operand. Runs adjoining a splice must be byte-aligned.
    /// Typecheck enforces it, so a violation here is a compiler bug
    /// surfaced as a diagnostic instead of a panic. A splice-free
    /// literal is one unbroken run and may legitimately be sub-byte
    /// (`Bits`).
    fn flush(
        &mut self,
        span: Span,
        ctx: &mut FnLowerCtx,
        block: IRBlockId,
        output: &mut LowerOutput,
    ) -> Result<(), ()> {
        if self.run.is_empty() {
            return Ok(());
        }
        if self.has_splice && !self.run_bits.is_multiple_of(8) {
            output.diagnostics.push(Diagnostic::error(
                "IR lower: fixed-width segments around a `Binary` splice must \
                 total whole bytes (typecheck should have rejected this literal)",
                span,
            ));
            return Err(());
        }
        let dest = emit_construct(std::mem::take(&mut self.run), self.run_bits, ctx, block);
        self.run_bits = 0;
        self.operands.push(dest);
        Ok(())
    }
}

/// Emit one `BinaryConstruct` over `segments`. The result is an
/// owned temp the consumer moves or releases.
fn emit_construct(
    segments: Vec<LoweredBinarySegment>,
    total_bits: u64,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let layout = ResolvedBinaryLayout {
        total_bits,
        byte_aligned: total_bits.is_multiple_of(8),
    };
    let dest_ty = if layout.byte_aligned {
        IRType::Binary
    } else {
        IRType::Bits
    };
    let dest = ctx.fresh_value(dest_ty);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryConstruct {
            dest,
            layout,
            segments,
        },
    );
    ctx.mark_owned(dest);
    dest
}

/// Fold the literal's operands (static-run constructs and spliced
/// values) into `Concat` instructions, mirroring `lower_string`'s
/// accumulator shape. A single operand just needs to be owned: a
/// run construct already is, and a lone borrowed splice clones so
/// the result is independent of the source binding.
fn concat_operands(operands: Vec<ValueId>, ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let mut rest = operands.into_iter();
    let Some(first) = rest.next() else {
        return emit_construct(Vec::new(), 0, ctx, block);
    };
    if rest.len() == 0 {
        return materialize_owned(ctx, block, first, &IRType::Binary);
    }
    rest.fold(first, |acc, operand| {
        let dest = ctx.fresh_value(IRType::Binary);
        ctx.cfg.append(
            block,
            IRInstruction::Concat {
                dest,
                kind: ConcatKind::Binary,
                lhs: acc,
                rhs: operand,
            },
        );
        ctx.mark_owned(dest);
        // `Concat` copies both operands, so owned intermediates are
        // dead after this step.
        drop_discarded_temp(ctx, block, acc);
        drop_discarded_temp(ctx, block, operand);
        dest
    })
}

pub(super) enum ClassifiedSegment {
    Integer { width: u64 },
    Float { width: u64 },
    Splice,
    String { byte_length: u64 },
}

/// Per-segment kind + width derivation. Mirrors typecheck's
/// `resolve_segment`, since both call sites need the same width
/// arithmetic to agree. The typecheck side rejects bad shapes while
/// this side assumes typecheck has already passed and just
/// transcribes. On a typecheck/lower mismatch we still surface a
/// diagnostic rather than panicking, so a regression surfaces as a
/// build-time error instead of a runtime crash.
///
/// `value_ty` is the lowered segment value's IR type, used to tell
/// a bare `Binary` splice from a bare 8-bit integer. Constant
/// folding classifies through the same function and passes `None`,
/// since constant segment values are literals and never splices.
pub(super) fn classify_segment(
    segment: &BinarySegment,
    literal_span: Span,
    value_ty: Option<&IRType>,
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
                "Binary" => ClassifiedSegment::Splice,
                "Float32" => ClassifiedSegment::Float { width: 32 },
                "Float64" => ClassifiedSegment::Float { width: 64 },
                "Int8" | "UInt8" => ClassifiedSegment::Integer { width: 8 },
                "Int16" | "UInt16" => ClassifiedSegment::Integer { width: 16 },
                "Int32" | "UInt32" => ClassifiedSegment::Integer { width: 32 },
                "Int64" | "UInt64" => ClassifiedSegment::Integer { width: 64 },
                other => {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "IR lower: unrecognized binary segment type annotation `{other}`, \
                             typecheck must have allowed an unsupported width",
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

    if matches!(value_ty, Some(IRType::Binary)) {
        return Ok(ClassifiedSegment::Splice);
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

pub(super) fn ast_endianness_to_ir(endian: Option<BinaryEndianness>) -> BinaryEndian {
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
