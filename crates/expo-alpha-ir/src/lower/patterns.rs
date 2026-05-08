//! Pattern lowering: walk a [`Pattern`] against a subject `ValueId`
//! and produce a [`PatternCheck`] describing whether the arm fires
//! unconditionally, after a runtime predicate, or after binding the
//! subject into a local slot. The match driver in
//! [`super::match_expr`] consumes the result to wire the gating
//! `CondBranch` and the per-arm body block.
//!
//! Today admits `Wildcard`, `Binding`, and primitive `Literal`
//! patterns; every other shape is a feature gap diagnosed in
//! typecheck and is unreachable here on the success path.

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::ResolvedType;
use expo_ast::labels::{pattern_kind_label, pattern_span};

use crate::function::{IRBlockId, IRInstruction};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use super::arms::lower_result_ty;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::ops::{const_value_type, lower_literal};

/// What the `match` driver needs to wire after lowering one arm's
/// pattern against the subject.
pub(super) enum PatternCheck {
    /// `_` or a binding pattern: the arm fires unconditionally. The
    /// driver routes the subject's open-flow block straight into
    /// the arm body block.
    CatchAll,
    /// A literal pattern: emit a `BinaryOp::Eq` against `subject`
    /// in `block` and gate the arm with the resulting `Bool`.
    Predicate { cond: ValueId },
}

/// Lower one pattern. Returns the open-flow block (each pattern
/// can append instructions: literals emit their constant + the
/// equality op, bindings emit a [`IRInstruction::LocalWrite`])
/// alongside the [`PatternCheck`] describing the gating shape.
pub(super) fn lower_pattern_check(
    pattern: &Pattern,
    subject: ValueId,
    subject_ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(PatternCheck, IRBlockId), ()> {
    match pattern {
        Pattern::Wildcard { .. } => Ok((PatternCheck::CatchAll, block)),
        Pattern::Binding { local_id, name, .. } => {
            let id = local_id.unwrap_or_else(|| {
                panic!(
                    "alpha IR lower: match binding `{name}` reaches lower without a stamped \
                     LocalId — typecheck resolve invariant violation",
                );
            });
            let ir_local = IRLocalId::from_local_id(id);
            if !ctx.local_is_declared(ir_local) {
                let ty = lower_result_ty(subject_ty, registry, output);
                let entry = ctx.entry_block();
                ctx.cfg.append(
                    entry,
                    IRInstruction::LocalDecl {
                        local: ir_local,
                        ty,
                    },
                );
                ctx.mark_local_declared(ir_local);
            }
            ctx.cfg.append(
                block,
                IRInstruction::LocalWrite {
                    local: ir_local,
                    value: subject,
                },
            );
            Ok((PatternCheck::CatchAll, block))
        }
        Pattern::Literal { value, span } => {
            let const_value = lower_literal(value, *span, &mut output.diagnostics)?;
            let const_ty = const_value_type(&const_value);
            let const_dest = ctx.fresh_value(const_ty.clone());
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest: const_dest,
                    value: const_value,
                },
            );
            let cond = ctx.fresh_value(IRType::Bool);
            ctx.cfg.append(
                block,
                IRInstruction::BinaryOp {
                    dest: cond,
                    lhs: subject,
                    op: crate::types::IRBinOp::Eq,
                    rhs: const_dest,
                },
            );
            Ok((PatternCheck::Predicate { cond }, block))
        }
        other => {
            output.diagnostics.push(Diagnostic::error(
                format!(
                    "alpha IR does not yet lower match pattern `{}`",
                    pattern_kind_label(other),
                ),
                pattern_span(other),
            ));
            Err(())
        }
    }
}
