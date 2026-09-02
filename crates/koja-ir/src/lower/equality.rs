//! Structural equality lowering for values with no nominal
//! `Equality` impl to call: tuples, unions, and function values.
//!
//! Nominal types get their `equals?` from `derive_equality` or a
//! hand-written impl. Tuples, unions, and closures have no decl to
//! hang one on, so [`lower_value_equality`] expands the comparison
//! inline at each site and recurses into each constituent's own
//! `equals?`. Every synthesized equality site in the lowering
//! (tuple `==`, union `==`, `f == g`, the `$eq_env$` closure glue)
//! routes through this one helper so the rule is written once.

use koja_ast::ast::{Arg, Expr};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::calls::conformance_method_symbol;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::ownership::drop_discarded_temp;
use super::package::resolved_type_to_ir_type;
use super::tuples::emit_tuple_get;
use super::unions::{
    UnionSubject, UnionSwitch, emit_int8_eq, emit_union_payload, emit_union_switch, emit_union_tag,
};
use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

/// Lower `receiver.equals?(other)` where the receiver is a tuple,
/// union, or function value. Typecheck admits exactly one argument.
pub(super) fn lower_equality_call(
    receiver: &Expr,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let [other] = args else {
        panic!(
            "IR lower: structural `equals?` reached lowering with {} args",
            args.len()
        );
    };
    let (lhs, current) = lower_expr(receiver, ctx, block, registry, output)?;
    let (rhs, current) = lower_expr(&other.value, ctx, current, registry, output)?;
    let (result, after) = lower_value_equality(
        lhs,
        rhs,
        &receiver.resolution,
        ctx,
        current,
        registry,
        output,
    );
    drop_discarded_temp(ctx, after, lhs);
    drop_discarded_temp(ctx, after, rhs);
    Ok((result, after))
}

/// Emit `lhs == rhs` for two values of type `ty`, returning the `Bool`
/// result and the block control continues in. Nominal types `Call`
/// their monomorphized `equals?`, tuples compare element-wise,
/// unions compare tag then payload, and functions compare by
/// [`IRInstruction::ClosureEquals`]. Both operands are borrowed.
pub(super) fn lower_value_equality(
    lhs: ValueId,
    rhs: ValueId,
    ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let structural = peel_alias(ty, registry);
    match &structural {
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            emit_tuple_eq(lhs, rhs, elements, ctx, block, registry, output)
        }
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) => {
            let ty = resolved_type_to_ir_type(&structural, registry, &mut output.instantiations);
            let dest = ctx.fresh_value(IRType::Bool);
            ctx.cfg
                .append(block, IRInstruction::ClosureEquals { dest, lhs, rhs, ty });
            (dest, block)
        }
        ResolvedType::Union(members) => {
            let union_ty =
                resolved_type_to_ir_type(&structural, registry, &mut output.instantiations);
            emit_union_eq(
                (lhs, rhs),
                (members, &union_ty),
                ctx,
                block,
                registry,
                output,
            )
        }
        _ => {
            let (callee, return_ty) =
                conformance_method_symbol(&structural, "equals?", 2, registry, output);
            let dest = ctx.fresh_value(return_ty);
            ctx.cfg.append(
                block,
                IRInstruction::Call {
                    dest,
                    callee,
                    args: vec![lhs, rhs],
                },
            );
            (dest, block)
        }
    }
}

/// Element-wise short-circuit equality: elements chain through a
/// [`Conjunction`] so element `equals?` calls after a mismatch never
/// run.
fn emit_tuple_eq(
    lhs: ValueId,
    rhs: ValueId,
    elements: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let Some(last) = elements.len().checked_sub(1) else {
        return (emit_bool_const(true, ctx, block), block);
    };
    let conjunction = Conjunction::new("tuple_eq", ctx);
    let mut current = block;
    for (index, element_ty) in elements.iter().enumerate() {
        let structural = peel_alias(element_ty, registry);
        let lhs_element = emit_tuple_get(lhs, index, &structural, ctx, current, registry, output);
        let rhs_element = emit_tuple_get(rhs, index, &structural, ctx, current, registry, output);
        let (cond, after) = lower_value_equality(
            lhs_element,
            rhs_element,
            &structural,
            ctx,
            current,
            registry,
            output,
        );
        if index == last {
            return conjunction.finish(cond, ctx, after);
        }
        current = conjunction.gate(cond, ctx, after);
    }
    unreachable!("tuple equality loop returns on its last element")
}

/// Tag-then-payload equality. A tag mismatch short-circuits to
/// `false`. Otherwise a switch on the left tag projects both payloads
/// as that member (valid because the tags agree) and recurses.
fn emit_union_eq(
    (lhs, rhs): (ValueId, ValueId),
    (members, union_ty): (&[ResolvedType], &IRType),
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let lhs_tag = emit_union_tag(lhs, union_ty, ctx, block);
    let rhs_tag = emit_union_tag(rhs, union_ty, ctx, block);
    let same_tag = emit_int8_eq(lhs_tag, rhs_tag, ctx, block);
    let conjunction = Conjunction::new("union_eq", ctx);
    let same_tag_block = conjunction.gate(same_tag, ctx, block);
    let switch = UnionSwitch {
        label: "union_eq",
        result_ty: IRType::Bool,
        subject: UnionSubject {
            members,
            ty: union_ty,
            value: lhs,
        },
    };
    let (payloads_equal, after) = emit_union_switch(
        switch,
        ctx,
        same_tag_block,
        registry,
        output,
        |arm, ctx, block, output| {
            let member_ir = ctx.type_of(arm.payload);
            let rhs_payload =
                emit_union_payload(rhs, arm.member_index, &member_ir, union_ty, ctx, block);
            lower_value_equality(
                arm.payload,
                rhs_payload,
                arm.member_ty,
                ctx,
                block,
                registry,
                output,
            )
        },
    );
    conjunction.finish(payloads_equal, ctx, after)
}

fn emit_bool_const(value: bool, ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::Bool);
    ctx.cfg.append(
        block,
        IRInstruction::Const {
            dest,
            value: ConstValue::Bool(value),
        },
    );
    dest
}

/// A short-circuit `and` chain under construction. Every step either
/// [`Self::gate`]s (false exits to the merge block early, true falls
/// into a fresh block) or [`Self::conclude`]s (its result is the
/// chain's result). The merge block declares one `Bool` param that
/// every incoming edge fills.
pub(super) struct Conjunction {
    merge: IRBlockId,
    result: ValueId,
}

impl Conjunction {
    pub(super) fn new(label: &str, ctx: &mut FnLowerCtx) -> Self {
        let merge = ctx.fresh_block(format!("{label}_merge"));
        let result = ctx.declare_merge_param(merge, IRType::Bool);
        Self { merge, result }
    }

    /// Continue only when `cond` holds. Returns the block a true
    /// `cond` falls into. False jumps to the merge with `false`.
    pub(super) fn gate(&self, cond: ValueId, ctx: &mut FnLowerCtx, block: IRBlockId) -> IRBlockId {
        let next = ctx.fresh_block("eq_next");
        let short_circuit = emit_bool_const(false, ctx, block);
        ctx.cfg.set_terminator(
            block,
            IRTerminator::CondBranch {
                cond,
                else_target: BranchTarget::with_args(self.merge, vec![short_circuit]),
                then_target: BranchTarget::to(next),
            },
        );
        next
    }

    /// Jump to the merge block carrying `cond` as the chain's result.
    pub(super) fn conclude(&self, cond: ValueId, ctx: &mut FnLowerCtx, block: IRBlockId) {
        ctx.cfg.set_terminator(
            block,
            IRTerminator::Branch(BranchTarget::with_args(self.merge, vec![cond])),
        );
    }

    /// [`Self::conclude`] the final step and hand back the result
    /// value plus the merge block control continues in.
    pub(super) fn finish(
        self,
        cond: ValueId,
        ctx: &mut FnLowerCtx,
        block: IRBlockId,
    ) -> (ValueId, IRBlockId) {
        self.conclude(cond, ctx, block);
        (self.result, self.merge)
    }
}
