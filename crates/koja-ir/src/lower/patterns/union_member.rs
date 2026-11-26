//! Union member pattern lowering. [`Pattern::UnionMember`] is the
//! wrapper that the typecheck puts around a variant or struct
//! pattern on a union subject.
//!
//! The check is one `tag == index` test for the member, then the
//! check of the inner pattern against the payload of that member.
//! The inner check reads the payload through a
//! [`BindOp::UnionPayload`] prefix and chains under
//! [`ChainMode::And`], the same as a pattern in an enum payload or a
//! struct field.
//!
//! A type argument can make the union collapse to the member, as
//! `Option<Int> | T` does at `T = Option<Int>`. The subject is then
//! the member itself, and the inner pattern lowers against it with no
//! tag test.

use koja_ast::ast::Pattern;
use koja_ast::identifier::ResolvedType;

use super::super::arms::emit_tag_eq;
use super::super::ctx::FnLowerCtx;
use super::super::package::resolved_type_to_ir_type;
use super::super::unions::emit_union_tag;
use super::structs::lower_subpattern_into;
use super::{
    BindOp, BindStep, ChainMode, PatternCheck, PatternInputs, TestStep, lower_pattern_check,
};
use crate::function::IRBlockId;
use crate::types::{IRType, ValueId};

pub(super) fn lower_union_member_check(
    member_type: &ResolvedType,
    pattern: &Pattern,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(PatternCheck, IRBlockId), ()> {
    let subject_ir = ctx.type_of(inputs.subject);
    let member_ir = resolved_type_to_ir_type(member_type, ctx.registry, ctx.output);
    let Some(member_index) = member_tag(&member_ir, &subject_ir, ctx) else {
        let member_inputs = PatternInputs {
            subject: inputs.subject,
            subject_ty: member_type,
        };
        return lower_pattern_check(pattern, member_inputs, ctx, block);
    };
    let member_test = emit_member_test(inputs.subject, &subject_ir, member_index, ctx, block);
    let mut steps = vec![member_test];
    let mut binds = Vec::new();
    let mut current_block = block;
    let payload = member_payload_step(member_index, &member_ir, &subject_ir);
    lower_subpattern_into(
        pattern,
        member_type,
        &member_ir,
        inputs.subject,
        payload,
        inputs,
        ctx,
        &mut current_block,
        &mut steps,
        &mut binds,
    );
    Ok((
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: binds,
            steps,
        },
        current_block,
    ))
}

/// Emit the test that `subject` holds the member at `member_index`
/// of the union `subject_ir`.
pub(super) fn emit_member_test(
    subject: ValueId,
    subject_ir: &IRType,
    member_index: u8,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> TestStep {
    let tag = emit_union_tag(subject, subject_ir, ctx, block);
    TestStep {
        cond: emit_tag_eq(tag, member_index, ctx, block),
        test_block: block,
    }
}

/// The step that reads the payload of the member at `member_index`
/// from a value of the union `subject_ir`.
pub(super) fn member_payload_step(
    member_index: u8,
    member_ir: &IRType,
    subject_ir: &IRType,
) -> BindStep {
    BindStep {
        op: BindOp::UnionPayload {
            member_index,
            member_type: member_ir.clone(),
            union_type: subject_ir.clone(),
        },
        output_type: member_ir.clone(),
    }
}

/// The tag of the member `member_ir` in the union `subject_ir`.
/// `None` when the subject is not a union, which is the collapsed
/// case of the module doc.
fn member_tag(member_ir: &IRType, subject_ir: &IRType, ctx: &FnLowerCtx<'_>) -> Option<u8> {
    let members = ctx.output.union_members(subject_ir)?;
    let index = members
        .iter()
        .position(|member| member == member_ir)
        .unwrap_or_else(|| {
            panic!(
                "IR lower: union member pattern names `{member_ir:?}`, which is not a \
                 member of subject union `{subject_ir:?}` (typecheck-resolve invariant \
                 violation)",
            )
        });
    Some(index as u8)
}
