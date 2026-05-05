//! Emit LLVM IR for one [`IRBasicBlock`] — instructions then
//! terminator. Each emitter takes a slice of context (LLVM context,
//! builder, the local SSA value map) and is otherwise pure.

use std::collections::BTreeMap;

use expo_alpha_ir::{ConstValue, IRBasicBlock, IRBinOp, IRInstruction, IRTerminator, ValueId};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::values::IntValue;

use crate::error::LlvmError;

/// Emit `block`'s instructions and terminator into the builder's
/// current insert position. The caller is responsible for having
/// called `position_at_end` on the block's LLVM target before this
/// runs.
pub(crate) fn emit_block<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    block: &IRBasicBlock,
) -> Result<(), LlvmError> {
    let mut values: BTreeMap<ValueId, IntValue<'ctx>> = BTreeMap::new();
    for instruction in &block.instructions {
        emit_instruction(context, builder, instruction, &mut values)?;
    }
    emit_terminator(builder, &block.terminator, &values)
}

fn emit_instruction<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    instruction: &IRInstruction,
    values: &mut BTreeMap<ValueId, IntValue<'ctx>>,
) -> Result<(), LlvmError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = emit_binary_op(builder, *op, lhs_value, rhs_value)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, .. } => Err(LlvmError::Codegen(format!(
            "alpha LLVM does not yet emit Call instructions \
             (callee `{callee}` at value {dest})",
        ))),
        IRInstruction::Const { dest, value } => {
            let constant = emit_const(context, value)?;
            values.insert(*dest, constant);
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, .. } => Err(LlvmError::Codegen(format!(
            "alpha LLVM does not yet emit unary {op:?} (at value {dest})",
        ))),
    }
}

/// Materialize a [`ConstValue`] as an LLVM `IntValue`. The slice's
/// seal pass admits `Bool` / `Int64` / `Unit` only; other variants
/// land here as feature-gap diagnostics so the IR vocabulary can grow
/// in lock-step with stdlib stubs without crashing the compiler.
fn emit_const<'ctx>(
    context: &'ctx Context,
    value: &ConstValue,
) -> Result<IntValue<'ctx>, LlvmError> {
    match value {
        ConstValue::Bool(b) => Ok(context.bool_type().const_int(u64::from(*b), false)),
        ConstValue::Int8(v) => Ok(context.i8_type().const_int(*v as u64, true)),
        ConstValue::Int16(v) => Ok(context.i16_type().const_int(*v as u64, true)),
        ConstValue::Int32(v) => Ok(context.i32_type().const_int(*v as u64, true)),
        ConstValue::Int64(v) => Ok(context.i64_type().const_int(*v as u64, true)),
        ConstValue::UInt8(v) => Ok(context.i8_type().const_int(u64::from(*v), false)),
        ConstValue::UInt16(v) => Ok(context.i16_type().const_int(u64::from(*v), false)),
        ConstValue::UInt32(v) => Ok(context.i32_type().const_int(u64::from(*v), false)),
        ConstValue::UInt64(v) => Ok(context.i64_type().const_int(*v, false)),
        ConstValue::Unit => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit constants in value position".to_string(),
        )),
    }
}

/// Wrapping integer arithmetic (no `nsw` / `nuw` flags) — Expo's
/// integer overflow contract is two's-complement wrap, matching v1
/// codegen's `build_int_add` / `_sub` / `_mul` calls.
fn emit_binary_op<'ctx>(
    builder: &Builder<'ctx>,
    op: IRBinOp,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let lowered = match op {
        IRBinOp::Add => builder.build_int_add(lhs, rhs, "add"),
        other => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit binary op `{other:?}`",
            )));
        }
    };
    lowered.map_err(|e| LlvmError::Codegen(format!("inkwell rejected build_int_add: {e}")))
}

fn emit_terminator<'ctx>(
    builder: &Builder<'ctx>,
    terminator: &IRTerminator,
    values: &BTreeMap<ValueId, IntValue<'ctx>>,
) -> Result<(), LlvmError> {
    match terminator {
        IRTerminator::Return { value: None } => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit-returning functions".to_string(),
        )),
        IRTerminator::Return { value: Some(id) } => {
            let return_value = lookup(values, *id)?;
            builder
                .build_return(Some(&return_value))
                .map(|_| ())
                .map_err(|e| LlvmError::Codegen(format!("inkwell rejected build_return: {e}")))
        }
    }
}

fn lookup<'ctx>(
    values: &BTreeMap<ValueId, IntValue<'ctx>>,
    id: ValueId,
) -> Result<IntValue<'ctx>, LlvmError> {
    values
        .get(&id)
        .copied()
        .ok_or_else(|| LlvmError::Codegen(format!("undefined SSA value {id} during lowering")))
}
