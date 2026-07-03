//! Per-block emission seams. Every IR block flows through exactly
//! one of these. Orchestrators choose between [`emit_block`] (the
//! convenient "instructions then terminator" path used by every
//! non-`main` walker) and the [`emit_instructions`] +
//! [`emit_terminator_default`] split (used by
//! [`crate::main_wrapper::emit_script_main`] so it can intercept the
//! terminator and inject the auto-print call before the natural
//! `ret`).
//!
//! Both seams accept a `values: &mut ValueMap` so callers can
//! pre-bind parameter [`ValueId`]s to LLVM `function.get_nth_param`
//! values before the body walk and so cross-block walks can thread
//! a single value map through every IR block. They also accept a
//! [`BlockMap`] so `Branch` / `CondBranch` terminators can resolve
//! their target [`IRBlockId`] to a real
//! [`inkwell::basic_block::BasicBlock`].
//!
//! # Module layout
//!
//! - This file: block seams, lookups, type aliases.
//! - [`instruction`]: per-instruction dispatch (`emit_instruction`).
//! - [`ops`]: binary + unary operator emission, parallel to
//!   `koja-ir-eval/src/ops.rs`.
//! - [`binary_construct`]: `BinaryConstruct` literal emission.
//! - [`calls`]: direct-call (`Call`) emission.
//! - [`closures`]: closure-shaped instructions (`MakeClosure`,
//!   `CallClosure`, `LoadCapture`) and the `IRType::Function` arm of
//!   `DropLocal`.
//! - [`concat`]: `Concat` emission.
//! - [`constants`]: `Const` and `LoadConst` emission.
//! - [`enums`]: `EnumConstruct`, `EnumTagGet`, `EnumPayloadFieldGet`.
//! - [`locals`]: `LocalDecl` / `LocalRead` / `LocalWrite` /
//!   `DropLocal` (heap arms).
//! - [`structs`]: `StructInit`, `FieldGet`.
//!
//! Type-creation pre-emit (struct + enum LLVM types from sealed IR
//! decls) lives in [`crate::layout`], not here. This module is
//! reserved for the IR-instruction-to-LLVM-instruction layer.

use std::collections::{BTreeMap, HashSet, VecDeque};

use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, IntValue, PhiValue};
use koja_ir::{
    BranchTarget, IRBasicBlock, IRBlockId, IRInstruction, IRTerminator, IRType, ValueId,
};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::{ir_basic_type, value_basic_type};

mod binary_construct;
mod binary_match;
mod calls;
mod clone;
pub(crate) mod closures;
pub(crate) mod collection_glue;
mod concat;
pub(crate) mod constants;
mod deep_copy;
pub(crate) mod enums;
pub(crate) mod heap_layout;
mod indirect;
mod instruction;
mod locals;
mod ops;
pub(crate) mod process;
mod structs;
mod unions;

pub(crate) use instruction::emit_instruction as emit_instruction_external;

/// Per-function SSA index. The migration to [`BasicValueEnum`] (from
/// `IntValue`) is what lets pointer-typed values (e.g. `IRType::String`
/// payload pointers) flow alongside ints. Op sites that need the int
/// narrow at the seam through [`lookup_int`].
pub(crate) type ValueMap<'ctx> = BTreeMap<ValueId, BasicValueEnum<'ctx>>;
pub(crate) type BlockMap<'ctx> = BTreeMap<IRBlockId, BasicBlock<'ctx>>;

/// Per-function index of `phi` instructions emitted for each
/// [`koja_ir::BlockParam`]. Branch terminators consult this
/// map post-`build_*_branch` to call `add_incoming` for every
/// (phi, branch-arg) pair on each [`BranchTarget`]. Empty for blocks
/// with no params (i.e. most blocks that aren't if/else/cond merges).
///
/// `None` entries in the per-block `Vec` are placeholders for
/// `IRType::Unit` block params: Unit has no value-level LLVM
/// representation (see [`crate::types::ir_basic_type`]'s `Unit`
/// arm), so we don't emit a phi and don't bind anything in the
/// `ValueMap`. The slot stays in the vec so the per-edge arg list
/// stays index-aligned with the target block's `params`, and the
/// terminator walk skips `None` slots without looking up the
/// corresponding arg.
pub(crate) type PhiMap<'ctx> = BTreeMap<IRBlockId, Vec<Option<PhiValue<'ctx>>>>;

/// Compute the set of [`IRBlockId`]s reachable from the entry block
/// (`blocks[0]`) via the terminator-edge graph. Used by the LLVM
/// emitters to short-circuit unreachable blocks: a value-producing
/// `if`/`else` whose arms both diverge synthesizes a merge block
/// that no edge feeds, and reading its `BlockParam` from the merge's
/// `Return` would fail because nothing materializes the param's
/// value at the LLVM level. The boundary's response is to skip the
/// natural terminator and emit `unreachable` for those blocks (see
/// [`emit_unreachable_terminator`]).
///
/// Empty `blocks` returns an empty set. One-block functions return
/// `{blocks[0].id}`.
///
/// Mirrors what `IRTerminator::Unreachable` would express more
/// directly once it lands alongside `Kernel.panic` and the other
/// `Never`-returning vocabulary. Until then this CFG walk is the
/// boundary's stand-in.
pub(crate) fn reachable_blocks(blocks: &[IRBasicBlock]) -> HashSet<IRBlockId> {
    let mut reachable = HashSet::new();
    let Some(entry) = blocks.first() else {
        return reachable;
    };
    reachable.insert(entry.id);
    let mut queue: VecDeque<IRBlockId> = VecDeque::from([entry.id]);
    while let Some(id) = queue.pop_front() {
        let Some(block) = blocks.iter().find(|b| b.id == id) else {
            continue;
        };
        for target in block_successors(block) {
            if reachable.insert(target) {
                queue.push_back(target);
            }
        }
    }
    reachable
}

/// Outgoing edges from `block` for the IR-level CFG walk. Most
/// blocks reach their successors only through the terminator, but
/// [`IRInstruction::Receive`] is a self-terminating instruction
/// whose arm + `after` body blocks are reached through the
/// dispatcher LLVM emits (see [`process::emit_receive`]). Counting
/// those as successors keeps the [`reachable_blocks`] walk in
/// agreement with what eventually executes. Without it the arm
/// bodies look unreachable and the LLVM emitter caps them with
/// `unreachable` instead of their natural body.
fn block_successors(block: &IRBasicBlock) -> Vec<IRBlockId> {
    let mut successors = terminator_successors(&block.terminator);
    for instr in &block.instructions {
        if let IRInstruction::Receive { after, arms, .. } = instr {
            successors.extend(arms.iter().map(|arm| arm.body));
            if let Some(after) = after {
                successors.push(after.body);
            }
        }
    }
    successors
}

fn terminator_successors(term: &IRTerminator) -> Vec<IRBlockId> {
    match term {
        IRTerminator::Branch(target) => vec![target.block],
        IRTerminator::CondBranch {
            else_target,
            then_target,
            ..
        } => vec![then_target.block, else_target.block],
        IRTerminator::Return { .. } | IRTerminator::TailCall { .. } | IRTerminator::Unreachable => {
            Vec::new()
        }
    }
}

/// Position the builder at `block_id`'s LLVM block and emit a single
/// `unreachable` instruction. Used as the substitute for the natural
/// terminator when a block is structurally unreachable in the CFG
/// (no incoming edges), where emitting the natural `Return` would
/// read values that the lowering boundary never materialized.
pub(crate) fn emit_unreachable_terminator<'ctx>(
    ctx: &EmitContext<'ctx>,
    block_id: IRBlockId,
    block_map: &BlockMap<'ctx>,
) -> Result<(), LlvmError> {
    let llvm_block = lookup_block(block_map, block_id)?;
    ctx.builder.position_at_end(llvm_block);
    ctx.builder.build_unreachable().or_ice().map(|_| ())
}

/// Emit `block` (instructions + terminator) into the builder's
/// current insert position. The caller is responsible for having
/// called `position_at_end` on the block's LLVM target before this
/// runs and for seeding `values` with any param `ValueId`s before
/// the entry-block walk.
///
/// `phi_map` maps each IR block to its pre-emitted block-param
/// phis (see [`declare_block_param_phis`]). The terminator walk
/// hands successor phis the values flowing along this block's
/// outgoing edges via `add_incoming`.
pub(crate) fn emit_block<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: &IRBasicBlock,
    block_map: &BlockMap<'ctx>,
    phi_map: &PhiMap<'ctx>,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    for instr in &block.instructions {
        emit_instruction_external(ctx, instr, values)?;
    }
    // `IRInstruction::Receive` is a self-terminating instruction:
    // the dispatcher in [`process::emit_receive`] ends the host
    // block with a `switch`/`br` into the arm body blocks before
    // the IR terminator (today: `Unreachable`) gets a chance to
    // run. Skip the natural terminator emit when the host block
    // is already capped so LLVM doesn't reject the duplicate.
    if let Some(insert_block) = ctx.builder.get_insert_block()
        && insert_block.get_terminator().is_some()
    {
        return Ok(());
    }
    emit_terminator_default(ctx, block.id, &block.terminator, values, block_map, phi_map)
}

/// Pre-pass over `blocks`: for every IR block declaring at least
/// one [`koja_ir::BlockParam`], position the builder at the
/// matching LLVM block and emit one `phi` instruction per param.
/// The phis form the join sites that branch terminators feed via
/// `add_incoming` later in [`emit_terminator_default`].
///
/// Each phi's `BasicValueEnum` is registered in `values` keyed by
/// the IR `BlockParam.dest`, so subsequent block-body emission
/// sees the param like any other operand. Phis for blocks with
/// zero params don't get emitted (and don't need to, since their
/// entry signature is empty), but the block still appears in the
/// returned [`PhiMap`] with an empty `Vec` so the terminator walk
/// can index uniformly.
pub(crate) fn declare_block_param_phis<'ctx>(
    ctx: &EmitContext<'ctx>,
    blocks: &[IRBasicBlock],
    block_map: &BlockMap<'ctx>,
    values: &mut ValueMap<'ctx>,
) -> Result<PhiMap<'ctx>, LlvmError> {
    let mut phi_map: PhiMap<'ctx> = PhiMap::new();
    for block in blocks {
        let mut phis: Vec<Option<PhiValue<'ctx>>> = Vec::with_capacity(block.params.len());
        if !block.params.is_empty() {
            let llvm_block = lookup_block(block_map, block.id)?;
            ctx.builder.position_at_end(llvm_block);
        }
        for (index, param) in block.params.iter().enumerate() {
            // Unit-typed params have no LLVM representation. Push a
            // placeholder so the per-edge wiring later skips both
            // the phi and the corresponding arg without index drift.
            // Eval handles Unit BlockParams natively (`Value::Unit`);
            // only LLVM needs this guard.
            if matches!(param.ty, IRType::Unit) {
                phis.push(None);
                continue;
            }
            let llvm_ty = ir_basic_type(ctx, &param.ty)?;
            let name = format!("param_{}_{}", block.id, index);
            let phi = ctx.builder.build_phi(llvm_ty, &name).or_ice()?;
            values.insert(param.dest, phi.as_basic_value());
            phis.push(Some(phi));
        }
        phi_map.insert(block.id, phis);
    }
    Ok(phi_map)
}

/// Emit `block`'s instructions only, returning a borrow of the
/// block's terminator so the caller can emit it (or substitute a
/// different one). The instruction walker mutates `values` in place.
/// Callers pass an owned map to avoid the borrow / aliasing
/// complications a `&mut` would cause when interleaved with the
/// returned terminator borrow.
pub(crate) fn emit_instructions<'ctx, 'block>(
    ctx: &EmitContext<'ctx>,
    block: &'block IRBasicBlock,
    seed: ValueMap<'ctx>,
) -> Result<(ValueMap<'ctx>, &'block IRTerminator), LlvmError> {
    let mut values = seed;
    for instr in &block.instructions {
        emit_instruction_external(ctx, instr, &mut values)?;
    }
    // Same self-terminating-Receive guard as [`emit_block`]. The
    // caller's terminator handling (today: `emit_main_return`)
    // checks for an already-capped block before running.
    Ok((values, &block.terminator))
}

/// Emit `terminator` to its natural LLVM form: `Return` -> `ret`,
/// `Branch` -> `br label %target`, `CondBranch` -> `br i1 %cond,
/// label %then, label %else`. After each branch, walk the target's
/// pre-declared block-param phis and call `add_incoming` for every
/// (phi, branch-arg) pair so the join values flow along this edge.
/// Branch targets resolve through the caller-provided `block_map`.
/// Misses are a compiler bug (the seal pass guarantees every
/// target is a registered IR block).
///
/// `pred` is the IR block id that owns `terminator`: the
/// "incoming block" we feed `add_incoming` for each successor's
/// phis.
pub(crate) fn emit_terminator_default<'ctx>(
    ctx: &EmitContext<'ctx>,
    pred: IRBlockId,
    terminator: &IRTerminator,
    values: &ValueMap<'ctx>,
    block_map: &BlockMap<'ctx>,
    phi_map: &PhiMap<'ctx>,
) -> Result<(), LlvmError> {
    match terminator {
        IRTerminator::Branch(target) => {
            let llvm_target = lookup_block(block_map, target.block)?;
            ctx.builder
                .build_unconditional_branch(llvm_target)
                .or_ice()?;
            wire_phi_incomings(ctx, target, pred, values, phi_map)
        }
        IRTerminator::CondBranch {
            cond,
            else_target,
            then_target,
        } => {
            let cond_value = lookup_int(values, *cond)?;
            let llvm_then = lookup_block(block_map, then_target.block)?;
            let llvm_else = lookup_block(block_map, else_target.block)?;
            ctx.builder
                .build_conditional_branch(cond_value, llvm_then, llvm_else)
                .or_ice()?;
            wire_phi_incomings(ctx, then_target, pred, values, phi_map)?;
            wire_phi_incomings(ctx, else_target, pred, values, phi_map)
        }
        IRTerminator::Return { value: None } => ctx.builder.build_return(None).or_ice().map(|_| ()),
        IRTerminator::Return { value: Some(id) } => {
            // A `Return { value: Some(id) }` against a Unit-typed slot
            // is the trailing-statement-of-a-Unit-fn shape: the IR
            // tracks the (unobservable) Unit value for seal /
            // dominator analysis, but LLVM's matching function type
            // is `void` and `ret void` ignores the SSA dest. Skipping
            // the `lookup` keeps a void-returning call's unregistered
            // dest from surfacing as "undefined SSA value".
            if current_function_returns_void(ctx) {
                ctx.builder.build_return(None).or_ice().map(|_| ())
            } else {
                let return_value = lookup(values, *id)?;
                ctx.builder
                    .build_return(Some(&return_value))
                    .or_ice()
                    .map(|_| ())
            }
        }
        IRTerminator::TailCall { args, .. } => emit_tail_call(ctx, args, values),
        IRTerminator::Unreachable => ctx.builder.build_unreachable().or_ice().map(|_| ()),
    }
}

/// Lower an [`IRTerminator::TailCall`] to the per-function TCO
/// scheme: store each `args[i]` into the matching parameter's
/// local slot, zero every body slot, then branch back to the
/// synthesized `tco_loop` header staged on
/// [`EmitContext::tco_frame`] by
/// [`crate::function::define_function`]. Reuses the already-
/// allocated entry-block alloca so there's no per-iteration stack
/// growth. The CFG just loops back through the same slots and
/// the body re-runs against fresh values.
///
/// The body-slot zeroing restores the fresh-activation invariant
/// the trailing exit drops rely on: those drops have already
/// released every slot's heap, so any slot the next iteration's
/// taken path doesn't re-declare must read as zero (a no-op drop)
/// at that iteration's exit, not as the stale released value.
///
/// The seal pass guarantees `args.len()` matches the function's
/// param arity. Missing the TCO frame here is a compiler bug
/// (define_function should have staged it whenever any block in
/// the function carries a `TailCall`).
fn emit_tail_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    args: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let frame = ctx.tco_frame().unwrap_or_else(|| {
        panic!(
            "LLVM emit: TailCall terminator emitted without a staged TCO frame \
             (define_function ordering violation)",
        )
    });
    if args.len() != frame.param_slots.len() {
        panic!(
            "LLVM emit: TailCall passes {} arg(s) but the function declares {} param(s) \
             (seal invariant violation)",
            args.len(),
            frame.param_slots.len(),
        );
    }
    for (arg, (local, _ty)) in args.iter().zip(frame.param_slots.iter()) {
        let value = lookup(values, *arg)?;
        let slot = ctx.local_slot(*local);
        ctx.builder.build_store(slot, value).or_ice()?;
    }
    for (local, ty) in &frame.body_slots {
        let llvm_ty = value_basic_type(ctx, ty)?;
        let slot = ctx.local_slot(*local);
        ctx.builder
            .build_store(slot, llvm_ty.const_zero())
            .or_ice()?;
    }
    ctx.builder
        .build_unconditional_branch(frame.loop_block)
        .or_ice()
        .map(|_| ())
}

/// For each non-`None` phi, look up the matching branch arg's LLVM
/// equivalent and hand it to the phi via `add_incoming`. `None`
/// slots correspond to Unit-typed [`koja_ir::BlockParam`]s
/// (no LLVM representation, no value-map binding), so we skip the
/// arg entirely without complaining when its lookup would fail.
///
/// The per-edge arity is checked at IR seal time, so a length
/// mismatch here is a compiler bug. We panic with a clear message
/// rather than surfacing a `Codegen` error the caller would have to
/// add a fallthrough for.
fn wire_phi_incomings<'ctx>(
    ctx: &EmitContext<'ctx>,
    target: &BranchTarget,
    pred: IRBlockId,
    values: &ValueMap<'ctx>,
    phi_map: &PhiMap<'ctx>,
) -> Result<(), LlvmError> {
    let phis = phi_map.get(&target.block).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing phi entry for block {} during branch-arg wiring",
            target.block,
        ))
    })?;
    if phis.len() != target.args.len() {
        panic!(
            "LLVM emit: branch from {pred} to {} passes {} arg(s) but target has {} \
             phi slot(s) (IR seal invariant violation)",
            target.block,
            target.args.len(),
            phis.len(),
        );
    }
    // The true predecessor is the builder's current block, not
    // `block_map[pred]`: they differ when an instruction splits its host
    // block mid-body (e.g. `BinaryMatch`'s length-guarded extraction).
    let pred_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen("phi incoming wiring with no active block".to_string())
    })?;
    for (phi, arg) in phis.iter().zip(target.args.iter()) {
        let Some(phi) = phi else {
            // Unit-typed BlockParam: no phi to wire, no LLVM value
            // for the arg to look up. The IR-level Const::Unit that
            // produced `arg` is structurally consistent at the
            // IR boundary. LLVM just elides the whole pair.
            continue;
        };
        let arg_value = lookup(values, *arg)?;
        phi.add_incoming(&[(&arg_value, pred_block)]);
    }
    Ok(())
}

pub(crate) fn lookup<'ctx>(
    values: &ValueMap<'ctx>,
    id: ValueId,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    values
        .get(&id)
        .copied()
        .ok_or_else(|| LlvmError::Codegen(format!("undefined SSA value {id} during emission")))
}

/// True when the LLVM function currently being defined has a `void`
/// return type. Used by the `Return` terminator emitter to drop the
/// trailing-Unit-value reference (the IR carries it, LLVM doesn't).
fn current_function_returns_void(ctx: &EmitContext<'_>) -> bool {
    let Some(block) = ctx.builder.get_insert_block() else {
        return false;
    };
    let Some(function) = block.get_parent() else {
        return false;
    };
    function.get_type().get_return_type().is_none()
}

/// Narrow [`lookup`] to an [`IntValue`] for op sites whose IR type is
/// guaranteed integer-family (binary / unary ops, branch conditions,
/// the int / bool printer paths). Misses surface as a codegen panic
/// because they indicate an upstream type-checker / lowering bug, not
/// a feature gap.
pub(crate) fn lookup_int<'ctx>(
    values: &ValueMap<'ctx>,
    id: ValueId,
) -> Result<IntValue<'ctx>, LlvmError> {
    Ok(lookup(values, id)?.into_int_value())
}

pub(super) fn lookup_block<'ctx>(
    block_map: &BlockMap<'ctx>,
    id: IRBlockId,
) -> Result<BasicBlock<'ctx>, LlvmError> {
    block_map
        .get(&id)
        .copied()
        .ok_or_else(|| LlvmError::Codegen(format!("undefined IR block {id} during emission")))
}
