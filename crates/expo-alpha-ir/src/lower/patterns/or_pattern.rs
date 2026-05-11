//! Or-pattern lowering: chain n alternatives through n-1 fresh
//! `match_or_alt_<n>` blocks, each producing one [`TestStep`].
//! The driver wires every step's success edge to the same body
//! block, every interior step's failure edge to the next step's
//! `test_block`, and the last step's failure edge to the
//! caller-supplied fall-through.
//!
//! Alternatives are restricted by typecheck to `Literal` /
//! `EnumUnit` (no bindings, no nested or-patterns); anything else
//! reaching here is a typecheck-resolve invariant violation.

use expo_ast::ast::Pattern;
use expo_ast::labels::pattern_kind_label;

use super::super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::emit_enum_tag_eq;
use super::literals::emit_literal_eq_or_panic;
use super::{PatternCheck, PatternInputs, TestStep};
use crate::function::IRBlockId;
use crate::types::ValueId;

pub(super) fn lower_or_check(
    alternatives: &[Pattern],
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> (PatternCheck, IRBlockId) {
    let mut steps = Vec::with_capacity(alternatives.len());
    let mut current = block;
    for (index, alternative) in alternatives.iter().enumerate() {
        if index > 0 {
            current = ctx.fresh_block(format!("match_or_alt_{index}"));
        }
        let cond = emit_or_alternative(alternative, inputs, ctx, current, output);
        steps.push(TestStep {
            cond,
            test_block: current,
        });
    }
    (
        PatternCheck::Tests {
            payload_binds: Vec::new(),
            steps,
        },
        current,
    )
}

fn emit_or_alternative(
    pattern: &Pattern,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    output: &mut LowerOutput,
) -> ValueId {
    match pattern {
        Pattern::EnumUnit { variant, .. } => emit_enum_tag_eq(variant, inputs, ctx, block, output),
        Pattern::Literal {
            literal_coercion,
            span,
            value,
        } => emit_literal_eq_or_panic(
            value,
            literal_coercion.as_ref(),
            *span,
            inputs.subject,
            ctx,
            block,
            output,
        ),
        other => panic!(
            "alpha IR lower: or-alternative `{}` reached lowering — \
             typecheck-resolve admits only Literal / EnumUnit alternatives",
            pattern_kind_label(other),
        ),
    }
}
