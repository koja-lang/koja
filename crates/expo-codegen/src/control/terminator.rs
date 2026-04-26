//! Shared emission for [`IRTerminator`] values.
//!
//! Every conditional construct's `emit_*` walker dispatches through
//! [`emit_terminator`] to translate an [`IRTerminator`] into LLVM
//! `br` / `cond_br` / `unreachable` calls. Emission walks the
//! terminator uniformly: it does **no** negation, does **no**
//! per-construct branch-direction adjustment, and does **no**
//! decision-making about which target represents "truthy" or "falsy"
//! -- the lowering pass already encoded that in the terminator's
//! `then` / `otherwise` slots. See [`expo_ir::blocks`] for the
//! canonicalization invariant this helper relies on.

use std::collections::HashMap;

use expo_ir::IRBlockId;
use expo_ir::blocks::IRTerminator;
use inkwell::basic_block::BasicBlock;
use inkwell::values::FunctionValue;

use crate::compiler::Compiler;
use crate::expr::compile_expr;

use super::coerce_to_bool;

/// Emit `terminator` into the builder's current block.
///
/// `block_map` resolves the function-scoped [`IRBlockId`]s carried on
/// the terminator into LLVM [`BasicBlock`] handles. Callers must
/// register every successor referenced by the terminator before
/// invoking this helper; an unknown id is treated as a hard error.
///
/// `function` is forwarded to [`compile_expr`] for the
/// [`IRTerminator::CondBranch`] case where the condition is still
/// carried as an AST stub.
///
/// The cond-branch case carries **no negation logic**: the `cond`
/// value is coerced straight into an i1 and routed to the terminator's
/// `then` and `otherwise` slots in their declared order. "unless-ness"
/// (and any other conditional construct's polarity) lives in the IR's
/// target ordering, not here.
pub(crate) fn emit_terminator<'ctx>(
    compiler: &mut Compiler<'ctx>,
    terminator: &IRTerminator,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    match terminator {
        IRTerminator::Branch(target) => {
            let target_bb = lookup(block_map, target)?;
            compiler
                .builder
                .build_unconditional_branch(target_bb)
                .unwrap();
            Ok(())
        }
        IRTerminator::CondBranch {
            cond,
            then,
            otherwise,
        } => {
            let cond_val = compile_expr(compiler, cond.as_ref(), function)?
                .ok_or("conditional terminator condition produced no value")?
                .value;
            let cond_int = coerce_to_bool(compiler, cond_val, "conditional terminator condition")?;
            let then_bb = lookup(block_map, then)?;
            let else_bb = lookup(block_map, otherwise)?;
            compiler
                .builder
                .build_conditional_branch(cond_int, then_bb, else_bb)
                .unwrap();
            Ok(())
        }
        IRTerminator::Unreachable => {
            compiler.builder.build_unreachable().unwrap();
            Ok(())
        }
    }
}

fn lookup<'ctx>(
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    id: &IRBlockId,
) -> Result<BasicBlock<'ctx>, String> {
    block_map
        .get(id)
        .copied()
        .ok_or_else(|| format!("emit_terminator: block id {:?} not in block_map", id))
}
