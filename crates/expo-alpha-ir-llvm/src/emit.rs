//! Emit LLVM IR for one [`IRBasicBlock`] — instructions then
//! terminator.
//!
//! Two seams are exposed so [`crate::compiler::Compiler::emit_as_main`]
//! can intercept the terminator (to inject the auto-print call):
//!
//! - [`emit_instructions`] walks `block.instructions` only and hands
//!   the values map plus a borrow of the terminator back to the
//!   caller.
//! - [`emit_terminator_default`] emits `Return` straight to LLVM's
//!   `ret`. Used by every non-`main` function.
//!
//! [`emit_block`] is the convenience composition of the two.
//!
//! Both seams accept a `seed: ValueMap` so callers can pre-bind
//! parameter `ValueId`s to the LLVM `function.get_nth_param` values
//! before the body walk starts. `emit_as_main` passes an empty
//! seed today; helper-function emission seeds one entry per
//! `IRFunctionParam`. They also accept a `&Module` so `Call`
//! instructions can resolve their callee by mangled name without
//! re-threading a separate function table.

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBinOp, IRInstruction, IRSymbol, IRTerminator, IRUnaryOp, ValueId,
};
use inkwell::IntPredicate;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, IntValue};

use crate::error::LlvmError;

pub(crate) type ValueMap<'ctx> = BTreeMap<ValueId, IntValue<'ctx>>;

/// Emit `block` (instructions + terminator) into the builder's
/// current insert position. The caller is responsible for having
/// called `position_at_end` on the block's LLVM target before this
/// runs and for seeding `seed` with any param `ValueId`s.
pub(crate) fn emit_block<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    module: &Module<'ctx>,
    block: &IRBasicBlock,
    seed: ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let (values, terminator) = emit_instructions(context, builder, module, block, seed)?;
    emit_terminator_default(builder, terminator, &values)
}

/// Emit `block`'s instructions only; return the populated value map
/// (starting from `seed`) plus a borrow of the block's terminator
/// so the caller can emit it (or substitute a different one).
pub(crate) fn emit_instructions<'ctx, 'block>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    module: &Module<'ctx>,
    block: &'block IRBasicBlock,
    seed: ValueMap<'ctx>,
) -> Result<(ValueMap<'ctx>, &'block IRTerminator), LlvmError> {
    let mut values = seed;
    for instruction in &block.instructions {
        emit_instruction(context, builder, module, instruction, &mut values)?;
    }
    Ok((values, &block.terminator))
}

/// Emit `terminator` to its natural LLVM form. Today that's just
/// `Return` → `ret`; branch / unconditional-jump terminators land
/// alongside multi-block emission.
pub(crate) fn emit_terminator_default<'ctx>(
    builder: &Builder<'ctx>,
    terminator: &IRTerminator,
    values: &ValueMap<'ctx>,
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

pub(crate) fn lookup<'ctx>(
    values: &ValueMap<'ctx>,
    id: ValueId,
) -> Result<IntValue<'ctx>, LlvmError> {
    values
        .get(&id)
        .copied()
        .ok_or_else(|| LlvmError::Codegen(format!("undefined SSA value {id} during emission")))
}

fn emit_instruction<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    module: &Module<'ctx>,
    instruction: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = emit_binary_op(builder, *op, lhs_value, rhs_value)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            let result = emit_call(builder, module, callee, args, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            let constant = emit_const(context, value)?;
            values.insert(*dest, constant);
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(values, *operand)?;
            let result = emit_unary_op(builder, *op, operand_value)?;
            values.insert(*dest, result);
            Ok(())
        }
    }
}

/// Emit a call to the helper function registered on `module` under
/// the callee's mangled symbol. Compiler declares every non-entry
/// function before any body emission and the IR seal pass guarantees
/// every `IRInstruction::Call::callee` resolves to a registered
/// function — so a miss here is a compiler bug, not a feature gap.
/// All return values are `IntValue` today (the seal pass admits only
/// `Bool` / `Int64` / `Unit`); `Unit` returns are rejected upstream
/// so we always have a basic value to extract.
fn emit_call<'ctx>(
    builder: &Builder<'ctx>,
    module: &Module<'ctx>,
    callee: &IRSymbol,
    args: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let mangled = callee.mangled();
    let function = module.get_function(mangled).unwrap_or_else(|| {
        panic!(
            "alpha LLVM emit: callee `{mangled}` not declared on the module — \
             declaration order or seal violation",
        )
    });
    let mut arg_values: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        arg_values.push(lookup(values, *arg)?.into());
    }
    let call_site = builder
        .build_call(function, &arg_values, "call")
        .map_err(|e| {
            LlvmError::Codegen(format!("inkwell rejected build_call for `{mangled}`: {e}"))
        })?;
    let basic_value = call_site.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "alpha LLVM does not yet emit Unit-returning calls (callee `{mangled}`)",
        ))
    })?;
    Ok(basic_value.into_int_value())
}

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

/// Integer arithmetic uses the wrapping `build_int_*` calls (no
/// `nsw`/`nuw`) per Expo's two's-complement overflow contract.
/// Comparisons use signed predicates — the seal pass admits `Int64`
/// only today; threading signedness for `UInt*` is a follow-up.
/// Match-arm order tracks `IRBinOp`'s declaration order in
/// [`expo_alpha_ir`].
fn emit_binary_op<'ctx>(
    builder: &Builder<'ctx>,
    op: IRBinOp,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let result = match op {
        IRBinOp::Add => builder.build_int_add(lhs, rhs, "add"),
        IRBinOp::And => builder.build_and(lhs, rhs, "and"),
        IRBinOp::Eq => builder.build_int_compare(IntPredicate::EQ, lhs, rhs, "eq"),
        IRBinOp::Gt => builder.build_int_compare(IntPredicate::SGT, lhs, rhs, "gt"),
        IRBinOp::GtEq => builder.build_int_compare(IntPredicate::SGE, lhs, rhs, "gte"),
        IRBinOp::Lt => builder.build_int_compare(IntPredicate::SLT, lhs, rhs, "lt"),
        IRBinOp::LtEq => builder.build_int_compare(IntPredicate::SLE, lhs, rhs, "lte"),
        IRBinOp::NotEq => builder.build_int_compare(IntPredicate::NE, lhs, rhs, "neq"),
        IRBinOp::Or => builder.build_or(lhs, rhs, "or"),
        IRBinOp::Div | IRBinOp::Mod | IRBinOp::Mul | IRBinOp::Sub => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit binary op `{op:?}`",
            )));
        }
    };
    result.map_err(|e| LlvmError::Codegen(format!("inkwell rejected emit for {op:?}: {e}")))
}

/// `Neg` wraps on `i64::MIN` (the eval interpreter's `checked_neg`
/// trap is a known divergence). `Not` is `xor x, -1`; the seal pass
/// only flows `Not` for `Bool`, so `i1` logical-not falls out for
/// free.
fn emit_unary_op<'ctx>(
    builder: &Builder<'ctx>,
    op: IRUnaryOp,
    operand: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let result = match op {
        IRUnaryOp::Neg => builder.build_int_neg(operand, "neg"),
        IRUnaryOp::Not => builder.build_not(operand, "not"),
    };
    result.map_err(|e| LlvmError::Codegen(format!("inkwell rejected emit for {op:?}: {e}")))
}
