//! Statement-list driver. [`lower_body`] threads the open
//! [`IRBlockId`] through each [`Statement`] in order, returning the
//! trailing [`FlowResult`] so callers (`lower_function` /
//! `lower_arm_into`) can decide how to wire the block's terminator.
//!
//! [`lower_body_to_blocks`] is the script-mode seam. It owns its own
//! [`FnLowerCtx`], so [`crate::lower_script`] doesn't need to know
//! about the lowering context at all.
//!
//! The fail-fast contract for feature-gap diagnostics lives here:
//! the moment any helper returns `Err(())`, the surrounding function
//! is dropped (matched against per-function fail-fast) and the
//! diagnostic propagates back to `lower_program` /
//! `lower_script` via the shared `diagnostics` accumulator.

use koja_ast::ast::{CompoundOp, Expr, LValue, Statement};
use koja_ast::identifier::{Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_typecheck::{GlobalKind, GlobalRegistry, StructDefinition, Substitution, substitute};

use crate::function::{
    BranchTarget, IRBasicBlock, IRBlockId, IRInstruction, IRSymbol, IRTerminator,
};
use crate::local::IRLocalId;
use crate::types::{IRBinOp, IRType, ValueId};

use super::ctx::{FlowResult, FnLowerCtx, LowerOutput};
use super::drops::emit_function_exit_drops;
use super::expr::lower_expr;
use super::ops::bin_op_result_type;
use super::ownership::{drop_discarded_temp, materialize_owned};
use super::package::resolved_type_to_ir_type;
use super::structs::resolved_struct_symbol;

/// Lower a sequence of statements into a CFG fragment, starting in a
/// fresh `entry` block. Used by [`crate::lower_script`] to lower a
/// script body without exposing [`FnLowerCtx`] outside the
/// [`crate::lower`] module tree.
///
/// `enclosing_symbol` seeds the synthesized-closure naming root for
/// any closures that surface inside the body. `lower_script`
/// passes a `<package>.__script_body` shape so script-body closures
/// get unique mangled names (`<package>.__script_body__closure0`).
/// `None` callers have no closures in scope (legacy / test-only).
///
/// `Err(())` means "a feature-gap diagnostic was already pushed and
/// the caller should drop this body / function from the surrounding
/// fragment". This matches the per-function fail-fast policy
/// `lower_program` already implements, and `lower_script` mirrors it for
/// the implicit script body.
pub(crate) fn lower_body_to_blocks(
    body: &[Statement],
    enclosing_symbol: Option<IRSymbol>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(Vec<IRBasicBlock>, IRType), ()> {
    let mut ctx = FnLowerCtx::new();
    if let Some(symbol) = enclosing_symbol {
        ctx.closures_mut().set_enclosing_symbol(symbol);
    }
    let entry = ctx.fresh_block("entry");
    let flow = lower_body(body, &mut ctx, entry, registry, output)?;
    let return_type = match &flow {
        FlowResult::Open {
            value: Some(id), ..
        } => ctx.type_of(*id),
        FlowResult::Open { value: None, .. } => IRType::Unit,
        // Closed flow means an explicit `return` exited the script.
        // Typecheck does not yet validate explicit return values (it
        // only checks the trailing expression), so their IR types
        // carry no guarantee. `Unit` is the defensible default until
        // explicit-return checking lands upstream.
        FlowResult::Closed => IRType::Unit,
    };
    finalize_open_flow(&mut ctx, flow, &return_type);
    Ok((ctx.into_blocks(), return_type))
}

/// Walk a sequence of statements, threading the open block through
/// each one. Returns the trailing statement's flow result. An
/// empty body returns `Open { value: None, block: entry }`.
pub(super) fn lower_body(
    body: &[Statement],
    ctx: &mut FnLowerCtx,
    mut block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    let mut last_value: Option<ValueId> = None;
    for stmt in body {
        // The previous statement's trailing value is now superseded.
        // If it owned a fresh heap-leaf allocation it's discarded, so
        // free it here (it is still live in the current `block`, which
        // its producer dominates). The final statement's value is not
        // dropped: it flows out as the body result.
        if let Some(discarded) = last_value.take() {
            drop_discarded_temp(ctx, block, discarded);
        }
        match lower_statement(stmt, ctx, block, registry, output)? {
            FlowResult::Open { value, block: next } => {
                last_value = value;
                block = next;
            }
            FlowResult::Closed => return Ok(FlowResult::Closed),
        }
    }
    Ok(FlowResult::Open {
        value: last_value,
        block,
    })
}

fn lower_statement(
    stmt: &Statement,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    match stmt {
        Statement::Expr(expr) => {
            let (value, next) = lower_expr(expr, ctx, block, registry, output)?;
            // A statement-position expression typed `Never` (today: a
            // call to `@intrinsic Kernel.panic`, signature rewritten by
            // the typecheck `override_divergent_return` pass) cannot
            // reach the next statement. Cap the open block with
            // `Unreachable` and report `Closed` so surrounding
            // arm-merge / fallthrough paths skip the would-be branch
            // edge. Without this, a match arm tail of `panic(...)`
            // emits a `Branch` with the `Unit`-typed call result as a
            // branch arg into a merge block whose param is the `T` the
            // other arms produce, which the LLVM emitter rejects with
            // an "undefined SSA value" since calls to void-returning
            // funcs don't register their dest in the value map.
            if is_never(&expr.resolution, registry) {
                ctx.cfg.set_terminator(next, IRTerminator::Unreachable);
                return Ok(FlowResult::Closed);
            }
            Ok(FlowResult::Open {
                value: Some(value),
                block: next,
            })
        }
        Statement::Return { value, .. } => {
            let return_value = match value.as_ref() {
                Some(expr) => {
                    let (id, next) = lower_expr(expr, ctx, block, registry, output)?;
                    // Acquire the result as an owned value *before* the
                    // exit drops free its source slots, so the return
                    // clone is taken while the source is live.
                    let return_ty = ctx.type_of(id);
                    let owned = materialize_owned(ctx, next, id, &return_ty);
                    emit_function_exit_drops(ctx, next);
                    ctx.cfg
                        .set_terminator(next, IRTerminator::Return { value: Some(owned) });
                    Some(owned)
                }
                None => {
                    emit_function_exit_drops(ctx, block);
                    ctx.cfg
                        .set_terminator(block, IRTerminator::Return { value: None });
                    None
                }
            };
            let _ = return_value;
            Ok(FlowResult::Closed)
        }
        Statement::Assignment { target, value, .. } => {
            lower_assignment(target, value, ctx, block, registry, output)
        }
        Statement::CompoundAssign {
            target, op, value, ..
        } => lower_compound_assignment(target, *op, value, ctx, block, registry, output),
        Statement::Break { span } => lower_break_statement(*span, ctx, block),
    }
}

/// Lower `break`: terminate the open block with a `Branch` to the
/// innermost enclosing loop's exit block. Typecheck-resolve has
/// already gated `break` on `loop_depth > 0`, so a missing exit on
/// the lower side is a resolver bug. Panic rather than ship a
/// half-baked IR fragment.
fn lower_break_statement(
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> Result<FlowResult, ()> {
    let exit = ctx.current_loop_exit().unwrap_or_else(|| {
        panic!(
            "IR lower: `break` reached lowering with no enclosing loop ({span:?}), \
             typecheck resolve invariant violation",
        )
    });
    // Escaping a match arm skips the arm tail's subject release, so
    // free any owned subject temps entered since the loop began.
    for value in ctx.subject_temps_since(exit.subject_temp_watermark) {
        let ty = ctx.type_of(value);
        ctx.cfg
            .append(block, IRInstruction::DropValue { value, ty });
    }
    ctx.cfg
        .set_terminator(block, IRTerminator::Branch(BranchTarget::to(exit.block)));
    Ok(FlowResult::Closed)
}

/// Lower a `Statement::Assignment` to (optional) `LocalDecl` + `LocalWrite`,
/// dispatching to [`lower_field_assignment`] for multi-segment field
/// writes (`p.x = …`).
///
/// Typecheck-resolve has already stamped the target with
/// [`Resolution::Local`] on its head (`LValue::local_id`), the
/// head's [`ResolvedType`] (`LValue::head_resolved_type`, multi-
/// segment only), and rejected pattern destructuring. This helper
/// assumes the well-typed shape and panics on deviation.
///
/// First write of a single-segment local emits a `LocalDecl` into
/// the function's entry block (regardless of which block the
/// assignment statement surface-syntactically lives in) so backends
/// see a single decl per slot at the canonical entry-block position.
/// Subsequent writes (and every multi-segment field write) just emit
/// the rebuild in the currently-open block.
///
/// Returns `Open { value: None, ... }` because assignment is
/// statement-level vocabulary. Its trailing value is the rhs's
/// [`ValueId`], but no surface syntax in this slice consumes it
/// directly. (Trailing-expression-of-body checking runs on the
/// trailing `Statement::Expr`, not on assignments.)
///
/// [`LocalId`]: koja_ast::identifier::LocalId
fn lower_assignment(
    lvalue: &LValue,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    if lvalue.segments.len() >= 2 {
        return lower_field_assignment(lvalue, value, ctx, block, registry, output);
    }

    let local_id = expect_local_id(lvalue);
    let ir_local = IRLocalId::from_local_id(local_id);

    let (value_id, current) = lower_expr(value, ctx, block, registry, output)?;
    let value_ty = ctx.type_of(value_id);

    // Acquire the rhs as an owned value: borrowed sources (literals,
    // reads, params) are deep-cloned so the slot holds an independent
    // heap-leaf allocation it can free at scope exit. The clone is
    // taken before the overwrite-drop, so a self-assign (`x = x`)
    // copies the old payload before freeing it.
    let owned_value = materialize_owned(ctx, current, value_id, &value_ty);

    if !ctx.local_is_declared(ir_local) {
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: value_ty.clone(),
            },
        );
        ctx.mark_local_declared(ir_local, value_ty.clone());
    } else if !ctx.local_is_live(ir_local) {
        // The slot was declared but fell out of the live set at a
        // loop or branch boundary. Skip the re-decl and the stale
        // drop. On a zero-trip path the slot is uninitialized, and
        // on a completed loop the back-edge already dropped the
        // last iteration's value. Re-mark it live so drop-glue
        // emission tracks the new value.
        ctx.mark_local_live(ir_local, value_ty.clone());
    } else if value_ty.is_heap_managed() {
        // Reassignment of a live heap-managed slot. Free the prior
        // owned value before overwriting so the old allocation
        // doesn't leak (a heap-leaf `rc--`, a composite `drop_T`).
        let stale = ctx.fresh_value(value_ty.clone());
        ctx.cfg.append(
            current,
            IRInstruction::LocalRead {
                dest: stale,
                local: ir_local,
                ty: value_ty.clone(),
            },
        );
        ctx.cfg.append(
            current,
            IRInstruction::DropValue {
                value: stale,
                ty: value_ty.clone(),
            },
        );
    }
    ctx.cfg.append(
        current,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: owned_value,
        },
    );
    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

/// Lower `head.f1.f2 = value` (any depth `>= 2`) into the SSA-pure
/// rebuild chain. `LocalRead` the head, `FieldGet` down each non-leaf
/// segment, lower and acquire the rhs (with a synthetic drop of the
/// overwritten heap-managed leaf), then `FieldSet` back up to the
/// root and `LocalWrite` the new root into the head slot.
///
/// The walker derives each segment's struct decl + field index by
/// substituting the previous level's `type_args` into the declared
/// field type, the same algorithm the resolver runs in
/// `walk_field_segments`, just one layer down (we look at IRTypes
/// for the actual `FieldGet` / `FieldSet` payloads).
fn lower_field_assignment(
    lvalue: &LValue,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    let local_id = expect_local_id(lvalue);
    let ir_local = IRLocalId::from_local_id(local_id);
    let head_ty = lvalue.head_resolved_type.clone().unwrap_or_else(|| {
        panic!(
            "IR lower: multi-segment assignment target `{}` carries no head \
             ResolvedType (typecheck resolve invariant violation)",
            lvalue.segments.join("."),
        )
    });

    let plan = build_field_chain(&head_ty, lvalue, registry, output);

    let head_ir_type = resolved_type_to_ir_type(&head_ty, registry, &mut output.instantiations);
    let root_value = ctx.fresh_value(head_ir_type.clone());
    ctx.cfg.append(
        block,
        IRInstruction::LocalRead {
            dest: root_value,
            local: ir_local,
            ty: head_ir_type.clone(),
        },
    );

    let mut parent_values = Vec::with_capacity(plan.len());
    let mut current_parent = root_value;
    for step in &plan[..plan.len().saturating_sub(1)] {
        let dest = ctx.fresh_value(step.field_ir_type.clone());
        ctx.cfg.append(
            block,
            IRInstruction::FieldGet {
                base: current_parent,
                dest,
                field_index: step.field_index,
                field_type: step.field_ir_type.clone(),
                struct_symbol: step.struct_symbol.clone(),
            },
        );
        parent_values.push(current_parent);
        current_parent = dest;
    }
    parent_values.push(current_parent);

    let leaf_step = plan
        .last()
        .expect("IR lower: field-assignment plan is empty for a multi-segment lvalue");
    let leaf_parent = parent_values[parent_values.len() - 1];

    let (rhs_value, current) = lower_expr(value, ctx, block, registry, output)?;

    // Acquire the rhs as an owned value. The rebuilt struct's field
    // must hold a reference its drop glue can release without
    // disturbing the source. The clone is taken before the
    // overwrite-drop, so a self-assign (`s.x = s.x`) copies the old
    // payload before freeing it, mirroring single-segment reassignment.
    let owned_rhs = materialize_owned(ctx, current, rhs_value, &leaf_step.field_ir_type);

    if leaf_step.field_ir_type.is_heap_managed() {
        let stale_leaf = ctx.fresh_value(leaf_step.field_ir_type.clone());
        ctx.cfg.append(
            current,
            IRInstruction::FieldGet {
                base: leaf_parent,
                dest: stale_leaf,
                field_index: leaf_step.field_index,
                field_type: leaf_step.field_ir_type.clone(),
                struct_symbol: leaf_step.struct_symbol.clone(),
            },
        );
        ctx.cfg.append(
            current,
            IRInstruction::DropValue {
                value: stale_leaf,
                ty: leaf_step.field_ir_type.clone(),
            },
        );
    }

    let mut new_value = owned_rhs;
    for (depth_from_leaf, step) in plan.iter().enumerate().rev() {
        let parent = parent_values[depth_from_leaf];
        let parent_ir_type = if depth_from_leaf == 0 {
            head_ir_type.clone()
        } else {
            plan[depth_from_leaf - 1].field_ir_type.clone()
        };
        let dest = ctx.fresh_value(parent_ir_type);
        ctx.cfg.append(
            current,
            IRInstruction::FieldSet {
                base: parent,
                dest,
                field_index: step.field_index,
                field_type: step.field_ir_type.clone(),
                struct_symbol: step.struct_symbol.clone(),
                value: new_value,
            },
        );
        new_value = dest;
    }

    ctx.cfg.append(
        current,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: new_value,
        },
    );

    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

/// One step in a field-assignment plan: the receiver struct's IR
/// symbol, the field's positional index, and the substituted field
/// IR type. Built by [`build_field_chain`] to bridge the resolver's
/// type-level walk to the IR's struct-symbol-and-index encoding.
struct FieldStep {
    field_index: u32,
    field_ir_type: IRType,
    struct_symbol: IRSymbol,
}

/// Walk `lvalue.segments[1..]` against the registry, mirroring the
/// resolver's `walk_field_segments`: at each step, look up the
/// receiver's struct definition, substitute the receiver's type-args
/// into the declared field type, and translate to an [`IRType`].
/// Returns one [`FieldStep`] per non-head segment.
fn build_field_chain(
    head_ty: &ResolvedType,
    lvalue: &LValue,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Vec<FieldStep> {
    let mut steps = Vec::with_capacity(lvalue.segments.len().saturating_sub(1));
    let mut current_ty = head_ty.clone();
    for segment in &lvalue.segments[1..] {
        let ResolvedType::Named {
            resolution: Resolution::Global(struct_id),
            type_args,
        } = &current_ty
        else {
            panic!(
                "IR lower: field-assignment segment `{segment}` projects through a \
                 non-struct receiver `{current_ty:?}`, typecheck resolve invariant \
                 violation",
            );
        };
        let struct_id = *struct_id;
        let definition = registry_struct(registry, struct_id);
        let (field_index, declared) = definition.lookup_field(segment).unwrap_or_else(|| {
            panic!(
                "IR lower: field-assignment segment `{segment}` is not a declared \
                 field on the receiver struct (typecheck resolve invariant violation)",
            )
        });
        let subst = Substitution::from_args(struct_id, type_args);
        let substituted = substitute(&declared.ty, &subst);
        let field_ir_type =
            resolved_type_to_ir_type(&substituted, registry, &mut output.instantiations);
        let struct_symbol =
            resolved_struct_symbol(&current_ty, registry, &mut output.instantiations);
        steps.push(FieldStep {
            field_index,
            field_ir_type,
            struct_symbol,
        });
        current_ty = substituted;
    }
    steps
}

/// Look up a struct's lifted definition, panicking on every shape
/// the resolver/seal already rejects upstream.
fn registry_struct(
    registry: &GlobalRegistry,
    struct_id: koja_ast::identifier::GlobalRegistryId,
) -> &StructDefinition {
    let entry = registry.get(struct_id).unwrap_or_else(|| {
        panic!("IR lower: struct id {struct_id} missing from registry (seal violation)",)
    });
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        panic!(
            "IR lower: registry id {struct_id} (`{}`) is not a struct with a lifted \
             definition (typecheck resolve invariant violation)",
            entry.identifier,
        );
    };
    definition
}

/// Lower `target op= value` to `LocalRead + (FieldGet*) + BinaryOp +
/// (FieldSet*) + LocalWrite`. Typecheck-resolve guarantees the head
/// local was already declared, the leaf field's type is arithmetic,
/// and the rhs's type matches, so this helper assumes a well-typed
/// shape and panics on deviation. Unlike [`lower_assignment`], we
/// never emit a `LocalDecl`, because compound assignment is
/// reassignment-only.
fn lower_compound_assignment(
    target: &LValue,
    op: CompoundOp,
    value: &Expr,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<FlowResult, ()> {
    let local_id = expect_local_id(target);
    let ir_local = IRLocalId::from_local_id(local_id);

    let (rhs, current) = lower_expr(value, ctx, block, registry, output)?;
    let ty = ctx.type_of(rhs);

    if target.segments.len() == 1 {
        let read_dest = ctx.fresh_value(ty.clone());
        ctx.cfg.append(
            current,
            IRInstruction::LocalRead {
                dest: read_dest,
                local: ir_local,
                ty: ty.clone(),
            },
        );
        let ir_op = compound_to_ir(op);
        let result = ctx.fresh_value(bin_op_result_type(ir_op, ty.clone()));
        ctx.cfg.append(
            current,
            IRInstruction::BinaryOp {
                dest: result,
                lhs: read_dest,
                op: ir_op,
                operand_ty: ty,
                rhs,
            },
        );
        ctx.cfg.append(
            current,
            IRInstruction::LocalWrite {
                local: ir_local,
                value: result,
            },
        );
        return Ok(FlowResult::Open {
            value: None,
            block: current,
        });
    }

    let head_ty = target.head_resolved_type.clone().unwrap_or_else(|| {
        panic!(
            "IR lower: multi-segment compound-assign target `{}` carries no head \
             ResolvedType (typecheck resolve invariant violation)",
            target.segments.join("."),
        )
    });
    let plan = build_field_chain(&head_ty, target, registry, output);
    let head_ir_type = resolved_type_to_ir_type(&head_ty, registry, &mut output.instantiations);

    let root_value = ctx.fresh_value(head_ir_type.clone());
    ctx.cfg.append(
        current,
        IRInstruction::LocalRead {
            dest: root_value,
            local: ir_local,
            ty: head_ir_type.clone(),
        },
    );

    let mut parent_values = Vec::with_capacity(plan.len());
    let mut current_parent = root_value;
    for step in &plan[..plan.len().saturating_sub(1)] {
        let dest = ctx.fresh_value(step.field_ir_type.clone());
        ctx.cfg.append(
            current,
            IRInstruction::FieldGet {
                base: current_parent,
                dest,
                field_index: step.field_index,
                field_type: step.field_ir_type.clone(),
                struct_symbol: step.struct_symbol.clone(),
            },
        );
        parent_values.push(current_parent);
        current_parent = dest;
    }
    parent_values.push(current_parent);

    let leaf_step = plan
        .last()
        .expect("IR lower: compound-assign plan is empty for a multi-segment lvalue");
    let leaf_parent = parent_values[parent_values.len() - 1];
    let leaf_value = ctx.fresh_value(leaf_step.field_ir_type.clone());
    ctx.cfg.append(
        current,
        IRInstruction::FieldGet {
            base: leaf_parent,
            dest: leaf_value,
            field_index: leaf_step.field_index,
            field_type: leaf_step.field_ir_type.clone(),
            struct_symbol: leaf_step.struct_symbol.clone(),
        },
    );

    let ir_op = compound_to_ir(op);
    let combined = ctx.fresh_value(bin_op_result_type(ir_op, leaf_step.field_ir_type.clone()));
    ctx.cfg.append(
        current,
        IRInstruction::BinaryOp {
            dest: combined,
            lhs: leaf_value,
            op: ir_op,
            operand_ty: leaf_step.field_ir_type.clone(),
            rhs,
        },
    );

    let mut new_value = combined;
    for (depth_from_leaf, step) in plan.iter().enumerate().rev() {
        let parent = parent_values[depth_from_leaf];
        let parent_ir_type = if depth_from_leaf == 0 {
            head_ir_type.clone()
        } else {
            plan[depth_from_leaf - 1].field_ir_type.clone()
        };
        let dest = ctx.fresh_value(parent_ir_type);
        ctx.cfg.append(
            current,
            IRInstruction::FieldSet {
                base: parent,
                dest,
                field_index: step.field_index,
                field_type: step.field_ir_type.clone(),
                struct_symbol: step.struct_symbol.clone(),
                value: new_value,
            },
        );
        new_value = dest;
    }

    ctx.cfg.append(
        current,
        IRInstruction::LocalWrite {
            local: ir_local,
            value: new_value,
        },
    );

    Ok(FlowResult::Open {
        value: None,
        block: current,
    })
}

/// True when `ty` is the registry-tracked `Global.Never` primitive.
/// Cheaper than threading a "divergent expression" flag down from
/// resolve, since typecheck already stamps `expr.resolution` with the
/// callee's return type, which for [`Kernel.panic`] is rewritten to
/// `Never` by the lift_signatures pass. Callees with no Never return
/// (the common case) early-out on the first guard.
fn is_never(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return false;
    };
    if !type_args.is_empty() {
        return false;
    }
    let never_id = match registry.lookup(&Identifier::new("Global", vec!["Never".to_string()])) {
        Some((id, _)) => id,
        None => return false,
    };
    *id == never_id
}

fn compound_to_ir(op: CompoundOp) -> IRBinOp {
    match op {
        CompoundOp::Add => IRBinOp::Add,
        CompoundOp::Div => IRBinOp::Div,
        CompoundOp::Mul => IRBinOp::Mul,
        CompoundOp::Sub => IRBinOp::Sub,
    }
}

/// Read the head [`LocalId`] off a sealed [`LValue`]. Typecheck-
/// resolve stamps both single-segment locals (`x`) and multi-segment
/// field-write head locals (`p` in `p.x = …`), so by the time
/// lowering runs the slot is non-`None` for every `LValue` that
/// reaches a backend.
fn expect_local_id(lvalue: &LValue) -> LocalId {
    lvalue.local_id.unwrap_or_else(|| {
        panic!(
            "IR lower: assignment target `{}` carries no LocalId (typecheck \
             resolve invariant violation)",
            lvalue.segments.join("."),
        )
    })
}

/// Wire a still-open trailing flow up to its function's `Return`.
/// Closed flows already set their own terminator (an inner `return`),
/// so there is nothing to do. Emits the function-exit drops, then stamps the
/// `Return` carrying the trailing value (if any).
pub(super) fn finalize_open_flow(ctx: &mut FnLowerCtx, flow: FlowResult, return_type: &IRType) {
    if let FlowResult::Open { value, block } = flow {
        if return_type == &IRType::Unit {
            if let Some(value) = value {
                drop_discarded_temp(ctx, block, value);
            }
            emit_function_exit_drops(ctx, block);
            ctx.cfg
                .set_terminator(block, IRTerminator::Return { value: None });
            return;
        }
        // Acquire the trailing value as owned *before* the exit drops,
        // so the return clone is taken while its source slots are live.
        let owned = value.map(|id| {
            let ty = ctx.type_of(id);
            materialize_owned(ctx, block, id, &ty)
        });
        emit_function_exit_drops(ctx, block);
        ctx.cfg
            .set_terminator(block, IRTerminator::Return { value: owned });
    }
}
