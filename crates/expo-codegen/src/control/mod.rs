//! Control flow compilation: if/else, cond, match, ternary, while loops,
//! for loops, and infinite loops with break support.

mod conditionals;
mod instructions;
mod loops;
mod patterns;
mod terminator;

use std::collections::HashMap;

use expo_ast::ast::Statement;
use expo_ir::IRBlockId;
use expo_ir::blocks::IRBasicBlock;
use expo_ir::values::IRValueId;

pub use conditionals::{compile_cond, compile_if, compile_ternary, compile_unless};
pub(crate) use instructions::{execute_instructions, maybe_typed_value};
pub use loops::{compile_for, compile_loop, compile_while};
pub use patterns::compile_match;
pub(crate) use patterns::{compile_pattern, get_payload_ptr, match_values};
pub(crate) use terminator::{emit_terminator, materialize_operand};

use expo_ir::IROperand;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::compiler::{Compiler, TypedValue};
use crate::expr::compile_expr;
use crate::stmt::compile_statement;

/// Walk a statement list and capture the value of the trailing
/// expression statement (if any). Per-construct conditional walkers
/// (`emit_unless` / `emit_if` / `emit_if_else` / `emit_cond` /
/// `emit_match_unified` / `emit_while_unified` / `emit_loop_unified`
/// / `emit_for_unified`) no longer call this helper -- their bodies
/// flow through [`expo_ir::Lowerer`] + [`execute_instructions`].
/// Two callers remain: closure bodies and `receive` arms, both of
/// which still walk AST statements directly because their lowering
/// hasn't reached the IR seam yet.
pub(crate) fn compile_body_as_value<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<TypedValue<'ctx>>, String> {
    let mut val: Option<TypedValue> = None;
    for (i, stmt) in body.iter().enumerate() {
        if compiler.current_block_terminated() {
            break;
        }
        if i == body.len() - 1
            && let Statement::Expr(expr) = stmt
        {
            val = compile_expr(compiler, expr, function)?;
            continue;
        }
        compile_statement(compiler, stmt, function)?;
    }
    Ok(val)
}

/// Append-and-register a fresh LLVM basic block onto `function`,
/// also recording the [`IRBlockId`] -> [`BasicBlock`] mapping into
/// [`crate::compiler::FnState::block_table`] so any
/// enclosing-construct terminator can resolve it via the fn-wide
/// fallback (see [`emit_terminator`]).
pub(crate) fn register_block<'ctx>(
    compiler: &mut Compiler<'ctx>,
    function: FunctionValue<'ctx>,
    id: IRBlockId,
    label: &str,
) -> BasicBlock<'ctx> {
    let bb = compiler.context.append_basic_block(function, label);
    compiler.fn_state.block_table.insert(id, bb);
    bb
}

/// Walk a sequence of [`IRBasicBlock`]s emitted by IR lowering.
///
/// Allocates one LLVM [`BasicBlock`] per [`IRBlockId`] (registered
/// into [`crate::compiler::FnState::block_table`] so cross-block
/// terminator references resolve), branches into the first block
/// from the current builder position (when not already terminated),
/// then iterates the blocks in source order: position the builder,
/// run [`execute_instructions`] against a fn-wide
/// [`IRValueId`]-keyed value map, and emit the terminator unless
/// the block self-terminated.
///
/// Returns the trailing [`BasicValueEnum`] mapped to `result_operand`
/// when present (used by Stub-execute-time control-flow shims to
/// plumb a merge-phi value back through `compile_expr`'s return).
/// When `result_operand` is `None` the construct is statement-shaped
/// and no value is captured.
pub(crate) fn walk_function_blocks<'ctx>(
    compiler: &mut Compiler<'ctx>,
    blocks: &[IRBasicBlock],
    closed: &std::collections::HashSet<IRBlockId>,
    function: FunctionValue<'ctx>,
    result_operand: Option<&IROperand>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    walk_function_blocks_seeded(compiler, blocks, closed, function, result_operand, &[])
}

/// Same as [`walk_function_blocks`] but pre-seeds the value map with
/// `(IRValueId, BasicValueEnum)` pairs. Used by `compile_match` to
/// thread the subject's LLVM pointer (allocated outside the CFG)
/// into the value map under the [`IROperand::Local`] id the IR-side
/// `lower_match_expr` references.
pub(crate) fn walk_function_blocks_seeded<'ctx>(
    compiler: &mut Compiler<'ctx>,
    blocks: &[IRBasicBlock],
    closed: &std::collections::HashSet<IRBlockId>,
    function: FunctionValue<'ctx>,
    result_operand: Option<&IROperand>,
    value_seeds: &[(IRValueId, BasicValueEnum<'ctx>)],
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if blocks.is_empty() {
        return Ok(None);
    }

    let mut block_map: HashMap<IRBlockId, BasicBlock<'ctx>> = HashMap::new();
    for blk in blocks {
        let bb = register_block(compiler, function, blk.id, &blk.label);
        block_map.insert(blk.id, bb);
    }

    let entry_bb = block_map[&blocks[0].id];
    if !compiler.current_block_terminated() {
        compiler
            .builder
            .build_unconditional_branch(entry_bb)
            .unwrap();
    }

    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    for (id, value) in value_seeds {
        value_map.insert(*id, *value);
    }
    for blk in blocks {
        let bb = block_map[&blk.id];
        compiler.builder.position_at_end(bb);
        execute_instructions(
            compiler,
            &blk.instructions,
            function,
            Some(&block_map),
            &mut value_map,
        )?;
        let end_bb = compiler.builder.get_insert_block().unwrap();
        // Emit the IR block's terminator BEFORE remapping
        // `block_map[blk.id]`. The terminator may target `blk.id`
        // itself (loop back-edge `Branch(loop_body)`); resolving
        // through the original `bb` produces the correct branch.
        // After Stub-deferred nested control flow, the builder is at
        // `end_bb` (potentially deeper than `bb`); the terminator
        // emits there but its target lookup uses `bb` for `blk.id`.
        if !compiler.current_block_terminated() && closed.contains(&blk.id) {
            emit_terminator(compiler, &blk.terminator, &block_map, &value_map, function)?;
        }
        // Now remap so subsequent blocks (e.g. a merge block whose
        // phi references this arm body's end LLVM block) see the
        // post-execution end block as the predecessor (mirrors
        // legacy `emit_match_unified`'s `block_map.insert(arm.body.id,
        // end_bb)` after walking each arm).
        if end_bb != bb {
            block_map.insert(blk.id, end_bb);
        }
    }

    let result = match result_operand {
        Some(operand) => Some(materialize_operand(compiler, operand, &value_map)?),
        None => None,
    };
    Ok(result)
}

/// Outcome of a [`lift_at_current`] call:
///
/// - `FallThrough`: the lift returned `Ok(None)` without emitting
///   any LLVM IR. The caller should run its legacy emission path.
/// - `Emitted(value)`: the lift emitted LLVM IR (and possibly
///   produced a value). `value` is the [`TypedValue`] result of the
///   lifted construct (or `None` when the construct returned void
///   or terminated all paths). The caller MUST NOT re-emit; it
///   should return the contained value (possibly `Ok(None)` for
///   void) directly.
pub(crate) enum LiftOutcome<'ctx> {
    FallThrough,
    Emitted(Option<crate::compiler::TypedValue<'ctx>>),
}

/// Run a "fragment lift" produced by an IR-side `lower_X_or_stub`
/// helper at the current LLVM builder position.
///
/// The lift may produce zero, one, or many blocks via the
/// [`expo_ir::CFGBuilder`] it's given:
///
/// - Zero blocks: lift returned `None` (fall-through to legacy).
/// - One block (the typical no-control-flow case): execute the
///   block's instructions on the current LLVM position and
///   materialize the operand in place. No LLVM blocks allocated.
/// - Multiple blocks: branch into the first block via
///   [`walk_function_blocks`], leaving the LLVM builder positioned
///   at the last walked block.
pub(crate) fn lift_at_current<'ctx, F>(
    compiler: &mut Compiler<'ctx>,
    function: FunctionValue<'ctx>,
    lift: F,
) -> Result<LiftOutcome<'ctx>, String>
where
    F: FnOnce(
        &mut expo_ir::Lowerer<'_>,
        &mut expo_ir::CFGBuilder,
        IRBlockId,
    ) -> Result<Option<(Option<IRBlockId>, IROperand, expo_ast::types::Type)>, String>,
{
    let mut builder = expo_ir::CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "lift_scratch");
    let lifted = {
        let mut lowerer = compiler.lowerer();
        lift(&mut lowerer, &mut builder, entry)?
    };
    let Some((_open, operand, return_type)) = lifted else {
        return Ok(LiftOutcome::FallThrough);
    };
    let (blocks, closed) = builder.into_blocks_with_closed();
    if blocks.len() == 1 {
        let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
        execute_instructions(
            compiler,
            &blocks[0].instructions,
            function,
            None,
            &mut value_map,
        )?;
        // If executing terminated the LLVM block (e.g. TCO rewrite
        // branched to `tco_loop`), the construct returned void by
        // construction.
        if compiler.current_block_terminated() {
            return Ok(LiftOutcome::Emitted(None));
        }
        let value = maybe_typed_value(compiler, &operand, &value_map, return_type)?;
        return Ok(LiftOutcome::Emitted(value));
    }
    // Multi-block fragment: walk via the function-blocks walker.
    let value = walk_function_blocks(compiler, &blocks, &closed, function, Some(&operand))?;
    Ok(LiftOutcome::Emitted(
        value.map(|v| TypedValue::new(v, return_type)),
    ))
}

/// Converts an integer value to a 1-bit bool. Already-boolean values pass
/// through; wider ints are compared != 0.
pub(super) fn coerce_to_bool<'ctx>(
    compiler: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<IntValue<'ctx>, String> {
    if !val.is_int_value() {
        return Err(format!("{label} must be a boolean"));
    }

    let iv = val.into_int_value();
    if iv.get_type().get_bit_width() == 1 {
        Ok(iv)
    } else {
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::NE, iv, iv.get_type().const_zero(), label)
            .unwrap())
    }
}
