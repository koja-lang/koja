//! Typed binding pattern lowering: `name: Type`.
//!
//! The bound type decides the shape of the check:
//!
//! - The binding holds the whole subject, as [`binds_whole_subject`]
//!   decides. It lowers as a plain binding with no test.
//! - The binding names one member of the subject union. It lowers to
//!   one `tag == index` test and one [`BindOp::UnionPayload`] bind.
//! - The binding names a union of members, as in `rest: B | C`
//!   against `A | B | C`. The arm fires when the subject carries none
//!   of the members that the binding leaves out. Each of those
//!   members is one `tag != index` test, and the bind is one
//!   [`BindOp::UnionNarrow`], which re-tags the subject into the
//!   bound union on the success edge.
//!
//! Each test chain is under [`ChainMode::And`], so the check nests
//! inside a struct, enum, or tuple pattern like every other test
//! chain.

use koja_ast::ast::Name;
use koja_ast::identifier::{LocalId, ResolvedType};

use super::super::arms::emit_tag_ne;
use super::super::ctx::FnLowerCtx;
use super::super::package::resolved_type_to_ir_type;
use super::super::unions::emit_union_tag;
use super::union_member::{emit_member_test, member_payload_step};
use super::{
    BindOp, BindStep, ChainMode, PatternCheck, PatternInputs, PayloadBind, TestStep,
    ensure_local_declared, lower_binding_check, require_local,
};
use crate::function::IRBlockId;
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

pub(super) fn lower_typed_binding_check(
    local_id: Option<LocalId>,
    name: &Name,
    resolved_type: &Option<ResolvedType>,
    inputs: &PatternInputs<'_>,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> (PatternCheck, IRBlockId) {
    let subject_ir = ctx.type_of(inputs.subject);
    let bound_ir = typed_binding_ir_type(name, resolved_type, ctx);
    if binds_whole_subject(&bound_ir, &subject_ir) {
        lower_binding_check(local_id, name, inputs, ctx, block);
        return (PatternCheck::CatchAll { binds: Vec::new() }, block);
    }
    let Some(members) = ctx.output.union_members(&subject_ir) else {
        panic!(
            "IR lower: typed-binding pattern `{name}` reaches lower with \
             non-Union subject `{subject_ir:?}`, typecheck-resolve invariant \
             violation",
        );
    };
    let member_index = members.iter().position(|member| member == &bound_ir);
    let local = require_local(local_id, name);
    match member_index {
        Some(index) => lower_member_check(
            local,
            index as u8,
            &bound_ir,
            inputs.subject,
            &subject_ir,
            ctx,
            block,
        ),
        // A type that is not one member names a union of members.
        None => lower_sub_union_check(local, &bound_ir, inputs.subject, &subject_ir, ctx, block),
    }
}

/// True when a typed binding of type `bound_ir` holds its whole
/// subject with no test. It does when the subject is not a union,
/// because the typecheck accepts it only when its type equals the
/// subject type. On a union subject, it does when it names that same
/// union.
pub(super) fn binds_whole_subject(bound_ir: &IRType, subject_ir: &IRType) -> bool {
    !matches!(subject_ir, IRType::Union(_)) || bound_ir == subject_ir
}

/// The IR type that a typed binding names.
pub(super) fn typed_binding_ir_type(
    name: &Name,
    resolved_type: &Option<ResolvedType>,
    ctx: &mut FnLowerCtx<'_>,
) -> IRType {
    let resolved = resolved_type.as_ref().unwrap_or_else(|| {
        panic!(
            "IR lower: typed-binding pattern `{name}` reaches lower without a \
             resolved_type (typecheck-resolve invariant violation)",
        );
    });
    resolved_type_to_ir_type(resolved, ctx.registry, ctx.output)
}

/// The check for a binding that names the member at `member_index`
/// of the subject union.
fn lower_member_check(
    local: IRLocalId,
    member_index: u8,
    member_ir: &IRType,
    subject: ValueId,
    subject_ir: &IRType,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> (PatternCheck, IRBlockId) {
    let member_test = emit_member_test(subject, subject_ir, member_index, ctx, block);
    ensure_local_declared(local, member_ir, ctx);
    ctx.mark_slot_borrowed(local);
    let bind = PayloadBind {
        local,
        chain: vec![member_payload_step(member_index, member_ir, subject_ir)],
    };
    (
        PatternCheck::Tests {
            chain_mode: ChainMode::And,
            payload_binds: vec![bind],
            steps: vec![member_test],
        },
        block,
    )
}

/// The check for a binding that names a union of members of the
/// subject union.
fn lower_sub_union_check(
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
