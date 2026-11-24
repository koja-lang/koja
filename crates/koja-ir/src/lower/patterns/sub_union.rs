//! Typed binding of a sub-union: `rest: B | C` against a subject of
//! type `A | B | C`.
//!
//! The arm fires when the subject carries none of the members that
//! the binding leaves out. Each of those members is one
//! `tag != index` test under [`ChainMode::And`], so the check nests
//! inside a struct, enum, or tuple pattern like every other test
//! chain. The bind is one [`BindOp::UnionNarrow`], which re-tags the
//! subject into the bound union on the success edge.

use super::super::arms::emit_tag_ne;
use super::super::ctx::FnLowerCtx;
use super::super::unions::emit_union_tag;
use super::{
    BindOp, BindStep, ChainMode, PatternCheck, PayloadBind, TestStep, ensure_local_declared,
};
use crate::function::IRBlockId;
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

pub(super) fn lower_sub_union_check(
    local: IRLocalId,
    bound_ir: &IRType,
    subject: ValueId,
    subject_ir: &IRType,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> (PatternCheck, IRBlockId) {
    let excluded = excluded_member_indexes(bound_ir, subject_ir, ctx);
    let tag = emit_union_tag(subject, subject_ir, ctx, block);
    let mut steps = Vec::with_capacity(excluded.len());
    let mut current = block;
    for (position, member_index) in excluded.into_iter().enumerate() {
        if position > 0 {
            current = ctx.fresh_block(format!("match_union_rest_{position}"));
        }
        steps.push(TestStep {
            cond: emit_tag_ne(tag, member_index, ctx, current),
            test_block: current,
        });
    }
    ensure_local_declared(local, bound_ir, ctx);
    ctx.mark_slot_borrowed(local);
    let bind = PayloadBind {
        local,
        chain: vec![BindStep {
            op: BindOp::UnionNarrow {
                source_type: subject_ir.clone(),
            },
            output_type: bound_ir.clone(),
        }],
    };
    (
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: vec![bind],
            steps,
        },
        current,
    )
}

/// Tag indexes of the subject members that the bound union does not
/// hold. Typecheck admits the pattern only when every bound member is
/// a subject member, and a binding of the whole subject never gets
/// here, so the result is never empty.
fn excluded_member_indexes(
    bound_ir: &IRType,
    subject_ir: &IRType,
    ctx: &FnLowerCtx<'_>,
) -> Vec<u8> {
    let Some(bound_members) = ctx.output.union_members(bound_ir) else {
        panic!(
            "IR lower: typed-binding type `{bound_ir:?}` is not a member of subject \
             union `{subject_ir:?}` (typecheck-resolve invariant violation)",
        );
    };
    let subject_members = ctx
        .output
        .union_members(subject_ir)
        .expect("IR lower: the subject of a sub-union check is a union");
    assert!(
        bound_members
            .iter()
            .all(|member| subject_members.contains(member)),
        "IR lower: typed-binding union `{bound_ir:?}` holds a member that is not in \
         subject union `{subject_ir:?}` (typecheck-resolve invariant violation)",
    );
    subject_members
        .iter()
        .enumerate()
        .filter(|(_, member)| !bound_members.contains(member))
        .map(|(index, _)| index as u8)
        .collect()
}
