//! Per-block emission seams. Every IR block flows through exactly
//! one of these; orchestrators choose between [`emit_block`] (the
//! convenient "instructions then terminator" path used by every
//! non-`main` walker) and the [`emit_instructions`] +
//! [`emit_terminator_default`] split (used by
//! [`crate::main_wrapper::emit_as_main`] so it can intercept the
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
//! - [`instruction`]: per-instruction dispatch (`emit_instruction`)
//!   plus the const + call helpers it routes to.
//! - [`ops`]: binary + unary operator emission, parallel to
//!   `expo-alpha-ir-eval/src/ops.rs`.
//!
//! Type-creation pre-emit (struct + enum LLVM types from sealed IR
//! decls) lives in [`crate::layout`], not here. This module is
//! reserved for the IR-instruction-to-LLVM-instruction layer.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::Display;

use expo_alpha_ir::{BranchTarget, IRBasicBlock, IRBlockId, IRTerminator, IRType, ValueId};
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, IntValue, PhiValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

mod instruction;
mod ops;

/// Per-function SSA index. The migration to [`BasicValueEnum`] (from
/// `IntValue`) is what lets pointer-typed values (e.g. `IRType::String`
/// payload pointers) flow alongside ints; op sites that need the int
/// narrow at the seam through [`lookup_int`].
pub(crate) type ValueMap<'ctx> = BTreeMap<ValueId, BasicValueEnum<'ctx>>;
pub(crate) type BlockMap<'ctx> = BTreeMap<IRBlockId, BasicBlock<'ctx>>;

/// Per-function index of `phi` instructions emitted for each
/// [`expo_alpha_ir::BlockParam`]. Branch terminators consult this
/// map post-`build_*_branch` to call `add_incoming` for every
/// (phi, branch-arg) pair on each [`BranchTarget`]. Empty for blocks
/// with no params (i.e. most blocks that aren't if/else/cond merges).
///
/// `None` entries in the per-block `Vec` are placeholders for
/// `IRType::Unit` block params: Unit has no value-level LLVM
/// representation (see [`crate::types::ir_basic_type`]'s `Unit`
/// arm), so we don't emit a phi and don't bind anything in the
/// `ValueMap`. The slot stays in the vec so the per-edge arg list
/// stays index-aligned with the target block's `params`; the
/// terminator walk skips `None` slots without looking up the
/// corresponding arg.
pub(crate) type PhiMap<'ctx> = BTreeMap<IRBlockId, Vec<Option<PhiValue<'ctx>>>>;

/// Wrap an inkwell builder error into [`LlvmError::Codegen`]. `op`
/// names the operation that failed (e.g. `"build_store"`,
/// `"build_call for `Foo`"`); pair with `format_args!` when the
/// operation needs runtime context.
pub(crate) fn inkwell_err(op: impl Display, e: impl Display) -> LlvmError {
    LlvmError::Codegen(format!("inkwell rejected {op}: {e}"))
}

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
/// Empty `blocks` returns an empty set; one-block functions return
/// `{blocks[0].id}`.
///
/// Mirrors what `IRTerminator::Unreachable` would express more
/// directly once it lands alongside `Kernel.panic` and the other
/// `Never`-returning vocabulary; until then this CFG walk is the
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
        for target in terminator_successors(&block.terminator) {
            if reachable.insert(target) {
                queue.push_back(target);
            }
        }
    }
    reachable
}

fn terminator_successors(term: &IRTerminator) -> Vec<IRBlockId> {
    match term {
        IRTerminator::Branch(target) => vec![target.block],
        IRTerminator::CondBranch {
            else_target,
            then_target,
            ..
        } => vec![then_target.block, else_target.block],
        IRTerminator::Return { .. } | IRTerminator::Unreachable => Vec::new(),
    }
}

/// Position the builder at `block_id`'s LLVM block and emit a single
/// `unreachable` instruction. Used as the substitute for the natural
/// terminator when a block is structurally unreachable in the CFG
/// (no incoming edges) — emitting the natural `Return` would read
/// values that the lowering boundary never materialized.
pub(crate) fn emit_unreachable_terminator<'ctx>(
    ctx: &EmitContext<'ctx>,
    block_id: IRBlockId,
    block_map: &BlockMap<'ctx>,
) -> Result<(), LlvmError> {
    let llvm_block = lookup_block(block_map, block_id)?;
    ctx.builder.position_at_end(llvm_block);
    ctx.builder
        .build_unreachable()
        .map(|_| ())
        .map_err(|e| inkwell_err("build_unreachable", e))
}

/// Emit `block` (instructions + terminator) into the builder's
/// current insert position. The caller is responsible for having
/// called `position_at_end` on the block's LLVM target before this
/// runs and for seeding `values` with any param `ValueId`s before
/// the entry-block walk.
///
/// `phi_map` maps each IR block to its pre-emitted block-param
/// phis (see [`declare_block_param_phis`]); the terminator walk
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
        instruction::emit_instruction(ctx, instr, values)?;
    }
    emit_terminator_default(ctx, block.id, &block.terminator, values, block_map, phi_map)
}

/// Pre-pass over `blocks`: for every IR block declaring at least
/// one [`expo_alpha_ir::BlockParam`], position the builder at the
/// matching LLVM block and emit one `phi` instruction per param.
/// The phis form the join sites that branch terminators feed via
/// `add_incoming` later in [`emit_terminator_default`].
///
/// Each phi's `BasicValueEnum` is registered in `values` keyed by
/// the IR `BlockParam.dest`, so subsequent block-body emission
/// sees the param like any other operand. Phis for blocks with
/// zero params don't get emitted (and don't need to — their entry
/// signature is empty), but the block still appears in the
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
            // Unit-typed params have no LLVM representation — push a
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
            let phi = ctx
                .builder
                .build_phi(llvm_ty, &name)
                .map_err(|e| inkwell_err(format_args!("build_phi for {}", param.dest), e))?;
            values.insert(param.dest, phi.as_basic_value());
            phis.push(Some(phi));
        }
        phi_map.insert(block.id, phis);
    }
    Ok(phi_map)
}

/// Emit `block`'s instructions only; return a borrow of the block's
/// terminator so the caller can emit it (or substitute a different
/// one). The instruction walker mutates `values` in place — callers
/// pass an owned map to avoid the borrow / aliasing complications a
/// `&mut` would cause when interleaved with the returned terminator
/// borrow.
pub(crate) fn emit_instructions<'ctx, 'block>(
    ctx: &EmitContext<'ctx>,
    block: &'block IRBasicBlock,
    seed: ValueMap<'ctx>,
) -> Result<(ValueMap<'ctx>, &'block IRTerminator), LlvmError> {
    let mut values = seed;
    for instr in &block.instructions {
        instruction::emit_instruction(ctx, instr, &mut values)?;
    }
    Ok((values, &block.terminator))
}

/// Emit `terminator` to its natural LLVM form: `Return` -> `ret`,
/// `Branch` -> `br label %target`, `CondBranch` -> `br i1 %cond,
/// label %then, label %else`. After each branch, walk the target's
/// pre-declared block-param phis and call `add_incoming` for every
/// (phi, branch-arg) pair so the join values flow along this edge.
/// Branch targets resolve through the caller-provided `block_map`;
/// misses are a compiler bug (the seal pass guarantees every
/// target is a registered IR block).
///
/// `pred` is the IR block id that owns `terminator` — the
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
                .map_err(|e| inkwell_err("build_unconditional_branch", e))?;
            wire_phi_incomings(target, pred, values, block_map, phi_map)
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
                .map_err(|e| inkwell_err("build_conditional_branch", e))?;
            wire_phi_incomings(then_target, pred, values, block_map, phi_map)?;
            wire_phi_incomings(else_target, pred, values, block_map, phi_map)
        }
        IRTerminator::Return { value: None } => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit-returning functions".to_string(),
        )),
        IRTerminator::Return { value: Some(id) } => {
            let return_value = lookup(values, *id)?;
            ctx.builder
                .build_return(Some(&return_value))
                .map(|_| ())
                .map_err(|e| inkwell_err("build_return", e))
        }
        IRTerminator::Unreachable => ctx
            .builder
            .build_unreachable()
            .map(|_| ())
            .map_err(|e| inkwell_err("build_unreachable", e)),
    }
}

/// For each non-`None` phi, look up the matching branch arg's LLVM
/// equivalent and hand it to the phi via `add_incoming`. `None`
/// slots correspond to Unit-typed [`expo_alpha_ir::BlockParam`]s
/// (no LLVM representation, no value-map binding); we skip the arg
/// entirely without complaining when its lookup would fail.
///
/// The per-edge arity is checked at IR seal time, so a length
/// mismatch here is a compiler bug — we panic with a clear message
/// rather than surfacing a `Codegen` error the caller would have to
/// add a fallthrough for.
fn wire_phi_incomings<'ctx>(
    target: &BranchTarget,
    pred: IRBlockId,
    values: &ValueMap<'ctx>,
    block_map: &BlockMap<'ctx>,
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
            "alpha LLVM emit: branch from {pred} to {} passes {} arg(s) but target has {} \
             phi slot(s) — IR seal invariant violation",
            target.block,
            target.args.len(),
            phis.len(),
        );
    }
    let pred_block = lookup_block(block_map, pred)?;
    for (phi, arg) in phis.iter().zip(target.args.iter()) {
        let Some(phi) = phi else {
            // Unit-typed BlockParam: no phi to wire, no LLVM value
            // for the arg to look up. The IR-level Const::Unit that
            // produced `arg` is structurally consistent at the
            // alpha-IR boundary; LLVM just elides the whole pair.
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
