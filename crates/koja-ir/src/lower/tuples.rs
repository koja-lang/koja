//! Anonymous-tuple literal construction, destructure lowering, and
//! inline conformance expansion.
//!
//! `(a, b)` literals lower to [`IRInstruction::TupleInit`] with each
//! element acquired as an owned value, mirroring struct literals.
//! `(x, y) = value` statements extract each element with
//! [`IRInstruction::TupleGet`], acquire it, and store it through the
//! same local-slot path as plain assignment, so a binding that names
//! an existing local rebinds that slot (dropping the stale value)
//! rather than declaring a new one. Destructure patterns are
//! irrefutable by typecheck (bindings, wildcards, and nested tuples
//! only), so no test blocks are ever minted here.
//!
//! Tuples have no nominal home for derived impls, so the universal
//! protocol functions (`format` / `print` / `inspect` / `equals?`) expand
//! inline at each call site instead: element-wise projection plus a
//! `Call` into each element's own conformance function, mirroring
//! what `derive_debug` / `derive_equality` synthesize for nominal
//! types. `equals?` routes through [`super::equality`], union
//! elements through [`super::unions`], and `format` renders closure
//! elements as `"..."`.

use koja_ast::ast::{Arg, Expr, Pattern};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_typecheck::{GlobalRegistry, peel_alias};

use super::body::store_owned_into_local;
use super::calls::{conformance_method_symbol, lower_debug_family};
use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::equality::lower_equality_call;
use super::expr::{emit_string_const, lower_expr};
use super::ownership::{drop_discarded_temp, materialize_owned};
use super::package::resolved_type_to_ir_type;
use super::unions::{UnionSubject, emit_union_format};
use crate::function::{IRBlockId, IRInstruction};
use crate::local::IRLocalId;
use crate::types::{ConcatKind, IRType, ValueId};

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

/// Lower `tuple.format()` / `print()` / `inspect()` / `equals?(other)`.
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
    if method == "equals?" {
        return lower_equality_call(receiver, args, ctx, block, registry, output);
    }
    let elements = tuple_element_resolutions(&receiver.resolution, registry);
    lower_debug_family(
        method,
        receiver,
        ctx,
        block,
        registry,
        output,
        |value, ctx, block, output| {
            emit_tuple_format(value, &elements, ctx, block, registry, output)
        },
    )
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

/// Build `"(" e0.format() ", " e1.format() ... ")"` as a `Concat`
/// chain, mirroring string-interpolation lowering. Union elements
/// branch on their tag, so the result may land in a later block.
pub(super) fn emit_tuple_format(
    value: ValueId,
    elements: &[ResolvedType],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let mut current = block;
    let mut acc = emit_string_const("(".to_string(), ctx, current);
    for (index, element_ty) in elements.iter().enumerate() {
        if index > 0 {
            let separator = emit_string_const(", ".to_string(), ctx, current);
            acc = emit_concat(acc, separator, ctx, current);
        }
        let (piece, after) =
            emit_element_format(value, index, element_ty, ctx, current, registry, output);
        current = after;
        acc = emit_concat(acc, piece, ctx, current);
    }
    let close = emit_string_const(")".to_string(), ctx, current);
    (emit_concat(acc, close, ctx, current), current)
}

/// Render element `index` through its own `format`. Function
/// elements have none and render as `"..."`, matching derived `Debug`.
fn emit_element_format(
    base: ValueId,
    index: usize,
    element_ty: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> (ValueId, IRBlockId) {
    let structural_element = peel_alias(element_ty, registry);
    if matches!(
        &structural_element,
        ResolvedType::Anonymous(AnonymousKind::Function { .. })
    ) {
        return (emit_string_const("...".to_string(), ctx, block), block);
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
    match &structural_element {
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            emit_tuple_format(extracted, elements, ctx, block, registry, output)
        }
        ResolvedType::Union(members) => {
            let union_ty = ctx.type_of(extracted);
            let subject = UnionSubject {
                members,
                ty: &union_ty,
                value: extracted,
            };
            emit_union_format(subject, ctx, block, registry, output)
        }
        _ => {
            let (callee, return_ty) =
                conformance_method_symbol(&structural_element, "format", 1, registry, output);
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
            (dest, block)
        }
    }
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

/// Project element `index` of a tuple value. Shared with the
/// structural equality lowering in [`super::equality`].
pub(super) fn emit_tuple_get(
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
