//! Inline conformance expansion for union values.
//!
//! A union has no decl to hang a derived impl on, so `format` /
//! `print` / `inspect` / `hash` (and `equals?`, via
//! [`super::equality`]) expand at each call site as a dispatch on
//! the tag byte: [`emit_union_switch`] projects the payload as the
//! matching member and lets the caller build one arm per member.
//! Every union conformance goes through that one switch so the
//! control-flow shape is written once.

use koja_ast::ast::{Arg, Expr};
use koja_ast::identifier::{AnonymousKind, Identifier, Resolution, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::calls::{conformance_method_symbol, lower_debug_family};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::{emit_string_const, lower_expr};
use super::ownership::{drop_discarded_temp, materialize_owned};
use super::package::resolved_type_to_ir_type;
use super::tuples::emit_tuple_format;
use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRBinOp, IRType, ValueId};

/// One member's view of a union value inside an [`emit_union_switch`]
/// arm. `payload` is the value projected as `member_ty`.
pub(super) struct UnionArm<'a> {
    pub(super) member_index: u8,
    pub(super) member_ty: &'a ResolvedType,
    pub(super) payload: ValueId,
}

/// A union value being switched over, with the member list and IR
/// type every arm needs to project its payload.
#[derive(Clone, Copy)]
pub(super) struct UnionSubject<'a> {
    pub(super) members: &'a [ResolvedType],
    pub(super) ty: &'a IRType,
    pub(super) value: ValueId,
}

/// Inputs to [`emit_union_switch`]: the subject plus the merge
/// param's type and the block label prefix.
pub(super) struct UnionSwitch<'a> {
    pub(super) label: &'a str,
    pub(super) result_ty: IRType,
    pub(super) subject: UnionSubject<'a>,
}

/// Lower `union.format()` / `print()` / `inspect()` / `hash()`.
/// Typecheck admits only these (plus `equals?`, intercepted earlier),
/// so anything else here is a resolve bug.
pub(super) fn lower_union_conformance_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    assert!(
        args.is_empty(),
        "IR lower: union `{method}` reached lowering with {} args",
        args.len()
    );
    let structural = peel_alias(&receiver.resolution, registry);
    let ResolvedType::Union(members) = &structural else {
        panic!(
            "IR lower: union conformance receiver resolved to `{structural:?}` \
             (typecheck resolve invariant violation)",
        );
    };
    let union_ty = resolved_type_to_ir_type(&structural, registry, &mut output.instantiations);
    if method == "hash" {
        let (value, current) = lower_expr(receiver, ctx, block, registry, output)?;
        let subject = UnionSubject {
            members,
            ty: &union_ty,
            value,
        };
        let (hash, after) = emit_union_hash(subject, ctx, current, registry, output);
        drop_discarded_temp(ctx, after, value);
        return Ok((hash, after));
    }
    lower_debug_family(
        method,
        receiver,
        ctx,
        block,
        registry,
        output,
        |value, ctx, block, output| {
            let subject = UnionSubject {
                members,
                ty: &union_ty,
                value,
            };
            emit_union_format(subject, ctx, block, registry, output)
        },
    )
}

/// Dispatch on the subject's tag. Arm `i` runs `arm` with the payload
/// projected as member `i`, and every arm's result flows into one
/// merge block param of `result_ty`. Returns that param and the
/// merge block control continues in.
pub(super) fn emit_union_switch(
    switch: UnionSwitch<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    mut arm: impl FnMut(
        UnionArm<'_>,
        &mut FnLowerCtx,
        IRBlockId,
        &mut LowerOutput,
    ) -> (ValueId, IRBlockId),
) -> (ValueId, IRBlockId) {
    let UnionSwitch {
        label,
        result_ty,
        subject:
            UnionSubject {
                members,
                ty: union_ty,
                value,
            },
    } = switch;
    let tag = emit_union_tag(value, union_ty, ctx, block);
    let merge = ctx.fresh_block(format!("{label}_merge"));
    let result = ctx.declare_merge_param(merge, result_ty);
    let last = members.len() - 1;
    let mut current = block;
    for (index, member_ty) in members.iter().enumerate() {
        let member_index = u8::try_from(index).expect("union member index fits the tag byte");
        let arm_block = if index == last {
            current
        } else {
            let expected = ctx.fresh_value(IRType::Int8);
            ctx.cfg.append(
                current,
                IRInstruction::Const {
                    dest: expected,
                    value: ConstValue::Int8(member_index as i8),
                },
            );
            let is_member = emit_int8_eq(tag, expected, ctx, current);
            let arm_block = ctx.fresh_block(format!("{label}_member"));
            let next = ctx.fresh_block(format!("{label}_next"));
            ctx.cfg.set_terminator(
                current,
                IRTerminator::CondBranch {
                    cond: is_member,
                    else_target: BranchTarget::to(next),
                    then_target: BranchTarget::to(arm_block),
                },
            );
            current = next;
            arm_block
        };
        let member_ir = resolved_type_to_ir_type(member_ty, registry, &mut output.instantiations);
        let payload = emit_union_payload(value, member_index, &member_ir, union_ty, ctx, arm_block);
        let (arm_result, after) = arm(
            UnionArm {
                member_index,
                member_ty,
                payload,
            },
            ctx,
            arm_block,
            output,
        );
        ctx.cfg.set_terminator(
            after,
            IRTerminator::Branch(BranchTarget::with_args(merge, vec![arm_result])),
        );
    }
    (result, merge)
}

/// Render the carried member through its own `format`. Function
/// members have none and render as `"..."`, matching derived `Debug`.
pub(super) fn emit_union_format(
    subject: UnionSubject<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let switch = UnionSwitch {
        label: "union_format",
        result_ty: IRType::String,
        subject,
    };
    emit_union_switch(
        switch,
        ctx,
        block,
        registry,
        output,
        |arm, ctx, block, output| {
            let member = peel_alias(arm.member_ty, registry);
            match &member {
                ResolvedType::Anonymous(AnonymousKind::Function { .. }) => {
                    let placeholder = emit_string_const("...".to_string(), ctx, block);
                    let owned = materialize_owned(ctx, block, placeholder, &IRType::String);
                    (owned, block)
                }
                ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
                    emit_tuple_format(arm.payload, elements, ctx, block, registry, output)
                }
                _ => {
                    let formatted = emit_conformance_call(
                        &member,
                        "format",
                        vec![arm.payload],
                        ctx,
                        block,
                        registry,
                        output,
                    );
                    (formatted, block)
                }
            }
        },
    )
}

/// Hash the carried member, then fold the tag in so two members with
/// equal payload hashes still land apart: `member.hash().bxor(tag.hash())`.
fn emit_union_hash(
    subject: UnionSubject<'_>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let int_ty = global_int_type(registry);
    let switch = UnionSwitch {
        label: "union_hash",
        result_ty: IRType::Int64,
        subject,
    };
    emit_union_switch(
        switch,
        ctx,
        block,
        registry,
        output,
        |arm, ctx, block, output| {
            let member = peel_alias(arm.member_ty, registry);
            let member_hash = emit_conformance_call(
                &member,
                "hash",
                vec![arm.payload],
                ctx,
                block,
                registry,
                output,
            );
            let tag = ctx.fresh_value(IRType::Int64);
            ctx.cfg.append(
                block,
                IRInstruction::Const {
                    dest: tag,
                    value: ConstValue::Int64(i64::from(arm.member_index)),
                },
            );
            let tag_hash =
                emit_conformance_call(&int_ty, "hash", vec![tag], ctx, block, registry, output);
            let mixed = emit_conformance_call(
                &int_ty,
                "bxor",
                vec![member_hash, tag_hash],
                ctx,
                block,
                registry,
                output,
            );
            (mixed, block)
        },
    )
}

fn global_int_type(registry: &GlobalRegistry) -> ResolvedType {
    let (id, _) = registry
        .lookup(&Identifier::new("Global", vec!["Int".to_string()]))
        .expect("`Global.Int` is registered before any body lowers");
    ResolvedType::leaf(Resolution::Global(id))
}

/// `Call` the monomorphized `<receiver>.method(args)`. Heap-managed
/// results are fresh temps the caller owns.
fn emit_conformance_call(
    receiver_ty: &ResolvedType,
    method: &str,
    args: Vec<ValueId>,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> ValueId {
    let (callee, return_ty) =
        conformance_method_symbol(receiver_ty, method, args.len(), registry, output);
    let owned = return_ty.is_heap_managed();
    let dest = ctx.fresh_value(return_ty);
    ctx.cfg
        .append(block, IRInstruction::Call { dest, callee, args });
    if owned {
        ctx.mark_owned(dest);
    }
    dest
}

pub(super) fn emit_union_tag(
    value: ValueId,
    union_ty: &IRType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let dest = ctx.fresh_value(IRType::Int8);
    ctx.cfg.append(
        block,
        IRInstruction::UnionTagGet {
            dest,
            ty: union_ty.clone(),
            value,
        },
    );
    dest
}

pub(super) fn emit_union_payload(
    value: ValueId,
    member_index: u8,
    member_ty: &IRType,
    union_ty: &IRType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let dest = ctx.fresh_value(member_ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::UnionPayloadGet {
            dest,
            member_index,
            member_type: member_ty.clone(),
            ty: union_ty.clone(),
            value,
        },
    );
    dest
}

pub(super) fn emit_int8_eq(
    lhs: ValueId,
    rhs: ValueId,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> ValueId {
    let dest = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::BinaryOp {
            dest,
            lhs,
            op: IRBinOp::Eq,
            operand_ty: IRType::Int8,
            rhs,
        },
    );
    dest
}
