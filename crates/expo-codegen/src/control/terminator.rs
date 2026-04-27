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
//!
//! Terminators reference values via [`IROperand`]. The walker
//! resolves operands to LLVM [`BasicValueEnum`] handles via:
//!
//! - inline literal variants → constant materialization on the
//!   builder's LLVM context, or
//! - [`IROperand::Local`] → lookup in the `value_map` populated by
//!   the construct's `emit_*` walker before it dispatches the
//!   terminator (typically by walking the block's `instructions`).

use std::collections::HashMap;

use expo_ir::IRBlockId;
use expo_ir::blocks::IRTerminator;
use expo_ir::values::{IROperand, IRValueId};
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;

use super::coerce_to_bool;

/// Emit `terminator` into the builder's current block.
///
/// `block_map` resolves the function-scoped [`IRBlockId`]s carried on
/// the terminator into LLVM [`BasicBlock`] handles. `value_map`
/// resolves [`IROperand::Local`] references to LLVM values produced
/// earlier in the same emission walker. Callers must register every
/// successor referenced by the terminator before invoking this
/// helper; an unknown id is treated as a hard error.
///
/// `function` is unused today but retained on the signature because
/// future operand materialization (e.g. inline literal struct /
/// enum constructors) will need a function context.
///
/// The cond-branch case carries **no negation logic**: the resolved
/// operand is coerced straight into an i1 and routed to the
/// terminator's `then` and `otherwise` slots in their declared
/// order. "unless-ness" (and any other conditional construct's
/// polarity) lives in the IR's target ordering, not here.
pub(crate) fn emit_terminator<'ctx>(
    compiler: &mut Compiler<'ctx>,
    terminator: &IRTerminator,
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
    _function: FunctionValue<'ctx>,
) -> Result<(), String> {
    match terminator {
        IRTerminator::Branch(target) => {
            let target_bb = lookup_block(block_map, target)?;
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
            let cond_val = materialize_operand(compiler, cond, value_map)?;
            let cond_int = coerce_to_bool(compiler, cond_val, "conditional terminator condition")?;
            let then_bb = lookup_block(block_map, then)?;
            let else_bb = lookup_block(block_map, otherwise)?;
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

/// Materialize an [`IROperand`] into an LLVM [`BasicValueEnum`].
///
/// Inline literal variants are lowered to backend constants on the
/// builder's LLVM context. [`IROperand::Local`] is resolved via
/// `value_map`, which the surrounding `emit_*` walker populates by
/// executing the block's instruction sequence before dispatching the
/// terminator.
pub(crate) fn materialize_operand<'ctx>(
    compiler: &Compiler<'ctx>,
    operand: &IROperand,
    value_map: &HashMap<IRValueId, BasicValueEnum<'ctx>>,
) -> Result<BasicValueEnum<'ctx>, String> {
    match operand {
        IROperand::ConstBool(b) => Ok(compiler
            .context
            .bool_type()
            .const_int(u64::from(*b), false)
            .into()),
        IROperand::ConstFloat(v) => Ok(compiler.context.f64_type().const_float(*v).into()),
        IROperand::ConstInt(v) => Ok(compiler
            .context
            .i64_type()
            .const_int(*v as u64, true)
            .into()),
        IROperand::ConstStr(_) => {
            Err("operand: string literals not yet materialized at the codegen seam".to_string())
        }
        IROperand::Local(id) => value_map
            .get(id)
            .copied()
            .ok_or_else(|| format!("materialize_operand: value id {id:?} not in value_map")),
        IROperand::Unit => {
            Err("operand: unit values not yet materialized at the codegen seam".to_string())
        }
    }
}

fn lookup_block<'ctx>(
    block_map: &HashMap<IRBlockId, BasicBlock<'ctx>>,
    id: &IRBlockId,
) -> Result<BasicBlock<'ctx>, String> {
    block_map
        .get(id)
        .copied()
        .ok_or_else(|| format!("emit_terminator: block id {id:?} not in block_map"))
}
