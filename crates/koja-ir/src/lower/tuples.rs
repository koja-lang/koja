//! Anonymous-tuple literal construction, destructure lowering, and
//! inline conformance expansion.
//!
//! `(a, b)` literals lower to [`IRInstruction::TupleInit`] with each
//! element acquired as an owned value, mirroring struct literals.
//! `(x, y) = value` statements extract each element with
//! [`IRInstruction::TupleGet`], acquire it, and store it through the
//! same local-slot path as plain assignment. Destructure patterns
//! are irrefutable by typecheck (bindings, wildcards, and nested
//! tuples only), so no test blocks are ever minted here.
//!
//! Tuples have no nominal home for derived impls, so the universal
//! protocol functions (`format` / `print` / `inspect` / `eq`) expand
//! inline at each call site instead: element-wise projection plus a
//! `Call` into each element's own conformance function, mirroring
//! what `derive_debug` / `derive_equality` synthesize for nominal
//! types (including the opaque-element fallbacks: closures and
//! unions render `"..."` in `format` and are skipped in `eq`).

use koja_ast::ast::{Arg, Expr, Pattern};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::body::store_owned_into_local;
use super::calls::{conformance_method_symbol, emit_io_puts};
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::expr::{emit_string_const, lower_expr};
use super::ownership::{drop_discarded_temp, materialize_owned};
use super::package::resolved_type_to_ir_type;
use crate::function::{BranchTarget, IRBlockId, IRInstruction, IRTerminator};
use crate::local::IRLocalId;
use crate::types::{ConcatKind, ConstValue, IRType, ValueId};

pub(super) fn lower_tuple_literal(
    elements: &[Expr],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let mut current = block;
    let mut values = Vec::with_capacity(elements.len());
    let mut types = Vec::with_capacity(elements.len());
    for element in elements {
        let (value, next) = lower_expr(element, ctx, current, registry, output)?;
        current = next;
        // Value semantics: an element store acquires an independent
        // value, same as a struct field init.
        let element_ty = ctx.type_of(value);
        let owned = materialize_owned(ctx, current, value, &element_ty);
        values.push(owned);
        types.push(element_ty);
    }
    let dest = ctx.fresh_value(IRType::Tuple(types.clone()));
    ctx.cfg.append(
        current,
        IRInstruction::TupleInit {
            dest,
            elements: values,
            ty: types,
        },
    );
    // A tuple literal owns its freshly acquired elements, so the
    // result is an owned temp, same as a struct literal.
    ctx.mark_owned(dest);
    Ok((dest, current))
}

/// Lower `(a, b) = value`. The value lowers once, each element is
/// extracted and stored, then the owned tuple temp is released
/// (element stores cloned what they keep, so the release only
/// drops the container's references).
pub(super) fn lower_destructure(
    pattern: &Pattern,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    let (tuple_value, current) = lower_expr(value, ctx, block, registry, output)?;
    let tuple_ty = ctx.type_of(tuple_value);
    bind_elements(pattern, tuple_value, &tuple_ty, ctx, current);
    if ctx.is_owned(tuple_value) && tuple_ty.is_heap_managed() {
        ctx.cfg.append(
            current,
            IRInstruction::DropValue {
                value: tuple_value,
                ty: tuple_ty,
            },
        );
    }
    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

/// Extract and store every bound element of one tuple level.
/// Intermediate nested-tuple projections stay borrowed, so only
/// the leaf stores clone.
fn bind_elements(
    pattern: &Pattern,
    base: ValueId,
    base_ty: &IRType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) {
    let Pattern::Tuple { elements, .. } = pattern else {
        panic!(
            "IR lower: destructure statement carries a non-tuple pattern \
             (parser invariant violation)",
        );
    };
    let IRType::Tuple(element_types) = base_ty else {
        panic!(
            "IR lower: destructure value lowered to `{base_ty:?}`, expected \
             IRType::Tuple (typecheck seal must have caught this)",
        );
    };
    for (index, (element_pattern, element_ty)) in elements.iter().zip(element_types).enumerate() {
        if matches!(element_pattern, Pattern::Wildcard { .. }) {
            continue;
        }
        let extracted = ctx.fresh_value(element_ty.clone());
        ctx.cfg.append(
            block,
            IRInstruction::TupleGet {
                base,
                dest: extracted,
                element_type: element_ty.clone(),
                index: index as u32,
            },
        );
        match element_pattern {
            Pattern::Binding { local_id, name, .. } => {
                let local_id = local_id.unwrap_or_else(|| {
                    panic!(
                        "IR lower: destructure binding `{name}` reaches lower without \
                         a local id (typecheck-resolve invariant violation)",
                    )
                });
                let ir_local = IRLocalId::from_local_id(local_id);
                let owned = materialize_owned(ctx, block, extracted, element_ty);
                store_owned_into_local(ctx, block, ir_local, owned, element_ty);
            }
            Pattern::Tuple { .. } => {
                bind_elements(element_pattern, extracted, element_ty, ctx, block)
            }
            other => panic!(
                "IR lower: destructure pattern contains a refutable element \
                 (`{other:?}`), typecheck-resolve invariant violation",
            ),
        }
    }
}

// --- conformance expansion ------------------------------------------

/// Lower `tuple.format()` / `print()` / `inspect()` / `eq(other)`.
/// Typecheck admits only these four, so anything else here is a
/// resolve bug.
pub(super) fn lower_tuple_conformance_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let elements = tuple_element_resolutions(&receiver.resolution, registry);
    let (receiver_value, mut current) = lower_expr(receiver, ctx, block, registry, output)?;
    match method {
        "eq" => {
            let [other] = args else {
                panic!(
                    "IR lower: tuple `eq` reached lowering with {} args",
                    args.len()
                );
            };
            let (other_value, after) = lower_expr(&other.value, ctx, current, registry, output)?;
            let (result, after) = emit_tuple_eq(
                receiver_value,
                other_value,
                &elements,
                ctx,
                after,
                registry,
                output,
            );
            drop_discarded_temp(ctx, after, receiver_value);
            drop_discarded_temp(ctx, after, other_value);
            Ok((result, after))
        }
        "format" => {
            let formatted =
                emit_tuple_format(receiver_value, &elements, ctx, current, registry, output);
            drop_discarded_temp(ctx, current, receiver_value);
            Ok((formatted, current))
        }
        "inspect" => {
            let formatted =
                emit_tuple_format(receiver_value, &elements, ctx, current, registry, output);
            current = emit_io_puts(formatted, ctx, current);
            drop_discarded_temp(ctx, current, formatted);
            // `inspect` returns the receiver unchanged so call
            // chains preserve the value.
            Ok((receiver_value, current))
        }
        "print" => {
            let formatted =
                emit_tuple_format(receiver_value, &elements, ctx, current, registry, output);
            current = emit_io_puts(formatted, ctx, current);
            drop_discarded_temp(ctx, current, formatted);
            drop_discarded_temp(ctx, current, receiver_value);
            let unit = ctx.fresh_value(IRType::Unit);
            ctx.cfg.append(
                current,
                IRInstruction::Const {
                    dest: unit,
                    value: ConstValue::Unit,
                },
            );
            Ok((unit, current))
        }
        other => panic!(
            "IR lower: tuple method `{other}` reached lowering \
             (typecheck resolve invariant violation)",
        ),
    }
}

fn tuple_element_resolutions(
    tuple_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Vec<ResolvedType> {
    let ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) = peel_alias(tuple_ty, registry)
    else {
        panic!(
            "IR lower: tuple conformance receiver resolved to `{tuple_ty:?}` \
             (typecheck resolve invariant violation)",
        );
    };
    elements
}

/// Closures and unions have no callable conformance functions, so
/// `format` renders their placeholder and `eq` skips them, matching
/// the derived-impl treatment of opaque struct fields.
fn is_opaque_element(ty: &ResolvedType) -> bool {
    matches!(
        ty,
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) | ResolvedType::Union(_)
    )
}

/// Build `"(" e0.format() ", " e1.format() ... ")"` as a `Concat`
/// chain, mirroring string-interpolation lowering.
fn emit_tuple_format(
    value: ValueId,
    elements: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> ValueId {
    let mut acc = emit_string_const("(".to_string(), ctx, block);
    for (index, element_ty) in elements.iter().enumerate() {
        if index > 0 {
            let separator = emit_string_const(", ".to_string(), ctx, block);
            acc = emit_concat(acc, separator, ctx, block);
        }
        let piece = emit_element_format(value, index, element_ty, ctx, block, registry, output);
        acc = emit_concat(acc, piece, ctx, block);
    }
    let close = emit_string_const(")".to_string(), ctx, block);
    emit_concat(acc, close, ctx, block)
}

fn emit_element_format(
    base: ValueId,
    index: usize,
    element_ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> ValueId {
    let structural_element = peel_alias(element_ty, registry);
    if is_opaque_element(&structural_element) {
        return emit_string_const("...".to_string(), ctx, block);
    }
    let extracted = emit_tuple_get(
        base,
        index,
        &structural_element,
        ctx,
        block,
        registry,
        output,
    );
    if let ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) = &structural_element {
        return emit_tuple_format(extracted, elements, ctx, block, registry, output);
    }
    let (callee, return_ty) =
        conformance_method_symbol(&structural_element, "format", registry, output);
    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        block,
        IRInstruction::Call {
            dest,
            callee,
            args: vec![extracted],
        },
    );
    ctx.mark_owned(dest);
    dest
}

/// `Concat` copies both operands, so owned intermediates are dead
/// after each step and freed immediately.
fn emit_concat(lhs: ValueId, rhs: ValueId, ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    let dest = ctx.fresh_value(IRType::String);
    ctx.cfg.append(
        block,
        IRInstruction::Concat {
            dest,
            kind: ConcatKind::String,
            lhs,
            rhs,
        },
    );
    ctx.mark_owned(dest);
    drop_discarded_temp(ctx, block, lhs);
    drop_discarded_temp(ctx, block, rhs);
    dest
}

/// Element-wise short-circuit equality. Comparable elements chain
/// through `CondBranch`es into a shared merge block (false exits
/// early), so element `eq` calls after a mismatch never run. A tuple
/// with no comparable elements is constant `true`.
fn emit_tuple_eq(
    lhs: ValueId,
    rhs: ValueId,
    elements: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let comparable: Vec<(usize, ResolvedType)> = elements
        .iter()
        .enumerate()
        .filter_map(|(index, ty)| {
            let structural = peel_alias(ty, registry);
            (!is_opaque_element(&structural)).then_some((index, structural))
        })
        .collect();
    if comparable.is_empty() {
        let dest = ctx.fresh_value(IRType::Bool);
        ctx.cfg.append(
            block,
            IRInstruction::Const {
                dest,
                value: ConstValue::Bool(true),
            },
        );
        return (dest, block);
    }
    let merge_block = ctx.fresh_block("tuple_eq_merge");
    let result = ctx.declare_merge_param(merge_block, IRType::Bool);
    let mut current = block;
    let last = comparable.len() - 1;
    for (position, (index, element_ty)) in comparable.into_iter().enumerate() {
        let (cond, after) = emit_element_eq(
            (lhs, rhs),
            index,
            &element_ty,
            ctx,
            current,
            registry,
            output,
        );
        current = after;
        if position == last {
            ctx.cfg.set_terminator(
                current,
                IRTerminator::Branch(BranchTarget::with_args(merge_block, vec![cond])),
            );
        } else {
            let next = ctx.fresh_block("tuple_eq_next");
            let short_circuit = ctx.fresh_value(IRType::Bool);
            ctx.cfg.append(
                current,
                IRInstruction::Const {
                    dest: short_circuit,
                    value: ConstValue::Bool(false),
                },
            );
            ctx.cfg.set_terminator(
                current,
                IRTerminator::CondBranch {
                    cond,
                    else_target: BranchTarget::with_args(merge_block, vec![short_circuit]),
                    then_target: BranchTarget::to(next),
                },
            );
            current = next;
        }
    }
    (result, merge_block)
}

fn emit_element_eq(
    (lhs, rhs): (ValueId, ValueId),
    index: usize,
    element_ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let lhs_element = emit_tuple_get(lhs, index, element_ty, ctx, block, registry, output);
    let rhs_element = emit_tuple_get(rhs, index, element_ty, ctx, block, registry, output);
    if let ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) = element_ty {
        return emit_tuple_eq(
            lhs_element,
            rhs_element,
            elements,
            ctx,
            block,
            registry,
            output,
        );
    }
    let (callee, return_ty) = conformance_method_symbol(element_ty, "eq", registry, output);
    let dest = ctx.fresh_value(return_ty);
    ctx.cfg.append(
        block,
        IRInstruction::Call {
            dest,
            callee,
            args: vec![lhs_element, rhs_element],
        },
    );
    (dest, block)
}

fn emit_tuple_get(
    base: ValueId,
    index: usize,
    element_ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> ValueId {
    let element_ir = resolved_type_to_ir_type(element_ty, registry, &mut output.instantiations);
    let extracted = ctx.fresh_value(element_ir.clone());
    ctx.cfg.append(
        block,
        IRInstruction::TupleGet {
            base,
            dest: extracted,
            element_type: element_ir,
            index: index as u32,
        },
    );
    extracted
}
