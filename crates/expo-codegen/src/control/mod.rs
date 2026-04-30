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
pub(crate) use terminator::emit_terminator;

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

/// Walk a body [`IRBasicBlock`]: execute its instructions and emit
/// its terminator iff the body has not already self-terminated
/// (e.g. via early `return` / `break` / `panic` inside
/// `body.instructions`). Builder must already be positioned at the
/// body's LLVM block.
pub(crate) fn walk_body<'ctx>(
    compiler: &mut Compiler<'ctx>,
    body: &IRBasicBlock,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    execute_instructions(compiler, &body.instructions, function, None, &mut value_map)?;
    if !compiler.current_block_terminated() {
        emit_terminator(compiler, &body.terminator, block_map, &value_map, function)?;
    }
    Ok(())
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
