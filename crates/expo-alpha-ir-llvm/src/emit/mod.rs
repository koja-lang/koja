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
//! - [`structs`]: pre-emit phase that mints LLVM `StructType`s for
//!   every [`expo_alpha_ir::IRStructDecl`] and registers them on
//!   the [`EmitCtx`] before any function emission walks an
//!   [`IRType::Struct`] reference.

use std::collections::BTreeMap;

use expo_alpha_ir::{IRBasicBlock, IRBlockId, IRTerminator, ValueId};
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, IntValue};

use crate::ctx::EmitCtx;
use crate::error::LlvmError;

mod instruction;
mod ops;
pub(crate) mod structs;

/// Per-function SSA index. The migration to [`BasicValueEnum`] (from
/// `IntValue`) is what lets pointer-typed values (e.g. `IRType::String`
/// payload pointers) flow alongside ints; op sites that need the int
/// narrow at the seam through [`lookup_int`].
pub(crate) type ValueMap<'ctx> = BTreeMap<ValueId, BasicValueEnum<'ctx>>;
pub(crate) type BlockMap<'ctx> = BTreeMap<IRBlockId, BasicBlock<'ctx>>;

/// Emit `block` (instructions + terminator) into the builder's
/// current insert position. The caller is responsible for having
/// called `position_at_end` on the block's LLVM target before this
/// runs and for seeding `values` with any param `ValueId`s before
/// the entry-block walk.
pub(crate) fn emit_block<'ctx>(
    ctx: &EmitCtx<'ctx>,
    block: &IRBasicBlock,
    block_map: &BlockMap<'ctx>,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    for instr in &block.instructions {
        instruction::emit_instruction(ctx, instr, values)?;
    }
    emit_terminator_default(ctx, &block.terminator, values, block_map)
}

/// Emit `block`'s instructions only; return a borrow of the block's
/// terminator so the caller can emit it (or substitute a different
/// one). The instruction walker mutates `values` in place — callers
/// pass an owned map to avoid the borrow / aliasing complications a
/// `&mut` would cause when interleaved with the returned terminator
/// borrow.
pub(crate) fn emit_instructions<'ctx, 'block>(
    ctx: &EmitCtx<'ctx>,
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
/// label %then, label %else`. Branch targets resolve through the
/// caller-provided `block_map`; misses are a compiler bug (the seal
/// pass guarantees every target is a registered IR block).
pub(crate) fn emit_terminator_default<'ctx>(
    ctx: &EmitCtx<'ctx>,
    terminator: &IRTerminator,
    values: &ValueMap<'ctx>,
    block_map: &BlockMap<'ctx>,
) -> Result<(), LlvmError> {
    match terminator {
        IRTerminator::Branch(target) => {
            let llvm_target = lookup_block(block_map, *target)?;
            ctx.builder
                .build_unconditional_branch(llvm_target)
                .map(|_| ())
                .map_err(|e| {
                    LlvmError::Codegen(format!("inkwell rejected build_unconditional_branch: {e}"))
                })
        }
        IRTerminator::CondBranch {
            cond,
            then_block,
            else_block,
        } => {
            let cond_value = lookup_int(values, *cond)?;
            let then_target = lookup_block(block_map, *then_block)?;
            let else_target = lookup_block(block_map, *else_block)?;
            ctx.builder
                .build_conditional_branch(cond_value, then_target, else_target)
                .map(|_| ())
                .map_err(|e| {
                    LlvmError::Codegen(format!("inkwell rejected build_conditional_branch: {e}"))
                })
        }
        IRTerminator::Return { value: None } => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit-returning functions".to_string(),
        )),
        IRTerminator::Return { value: Some(id) } => {
            let return_value = lookup(values, *id)?;
            ctx.builder
                .build_return(Some(&return_value))
                .map(|_| ())
                .map_err(|e| LlvmError::Codegen(format!("inkwell rejected build_return: {e}")))
        }
    }
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
