//! Per-instruction dispatch + the const + call helpers it routes
//! to. Operator emission lives in the sibling [`super::ops`] module.

use expo_alpha_ir::{ConstValue, IRInstruction, IRSymbol, ValueId};
use inkwell::values::{BasicMetadataValueEnum, IntValue};

use crate::ctx::EmitCtx;
use crate::error::LlvmError;

use super::{ValueMap, lookup, ops};

pub(super) fn emit_instruction<'ctx>(
    ctx: &EmitCtx<'ctx>,
    instr: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match instr {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = ops::emit_binary_op(ctx, *op, lhs_value, rhs_value)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            let result = emit_call(ctx, callee, args, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            // `Unit` has no machine-level representation — the
            // merge block of an `if` / `unless` emits a `Const::Unit`
            // as the conditional's "value" so the surrounding
            // expression has a `ValueId` to thread, but in practice
            // nothing downstream consumes it (the if/unless
            // expression is Unit-typed and the function's actual
            // return is the trailing Int / Bool expression that
            // follows the conditional). Skip emission entirely; if
            // some caller does reference the Unit value, the
            // resulting `lookup` miss surfaces as an explicit
            // "undefined SSA value" error rather than a half-shaped
            // i1 / i8 placeholder that papers over a real feature
            // gap.
            if matches!(value, ConstValue::Unit) {
                return Ok(());
            }
            let constant = emit_const(ctx, value)?;
            values.insert(*dest, constant);
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(values, *operand)?;
            let result = ops::emit_unary_op(ctx, *op, operand_value)?;
            values.insert(*dest, result);
            Ok(())
        }
    }
}

/// Emit a call to the helper function registered on `ctx.module`
/// under the callee's mangled symbol. Every non-entry function is
/// declared before any body emission and the IR seal pass guarantees
/// every `IRInstruction::Call::callee` resolves to a registered
/// function — so a miss here is a compiler bug, not a feature gap.
/// All return values are `IntValue` today (the seal pass admits only
/// `Bool` / `Int64` / `Unit`); `Unit` returns are rejected upstream
/// so we always have a basic value to extract.
fn emit_call<'ctx>(
    ctx: &EmitCtx<'ctx>,
    callee: &IRSymbol,
    args: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let mangled = callee.mangled();
    let function = ctx.module.get_function(mangled).unwrap_or_else(|| {
        panic!(
            "alpha LLVM emit: callee `{mangled}` not declared on the module — \
             declaration order or seal violation",
        )
    });
    let mut arg_values: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        arg_values.push(lookup(values, *arg)?.into());
    }
    let call_site = ctx
        .builder
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

fn emit_const<'ctx>(ctx: &EmitCtx<'ctx>, value: &ConstValue) -> Result<IntValue<'ctx>, LlvmError> {
    match value {
        ConstValue::Bool(b) => Ok(ctx.context.bool_type().const_int(u64::from(*b), false)),
        ConstValue::Int8(v) => Ok(ctx.context.i8_type().const_int(*v as u64, true)),
        ConstValue::Int16(v) => Ok(ctx.context.i16_type().const_int(*v as u64, true)),
        ConstValue::Int32(v) => Ok(ctx.context.i32_type().const_int(*v as u64, true)),
        ConstValue::Int64(v) => Ok(ctx.context.i64_type().const_int(*v as u64, true)),
        ConstValue::UInt8(v) => Ok(ctx.context.i8_type().const_int(u64::from(*v), false)),
        ConstValue::UInt16(v) => Ok(ctx.context.i16_type().const_int(u64::from(*v), false)),
        ConstValue::UInt32(v) => Ok(ctx.context.i32_type().const_int(u64::from(*v), false)),
        ConstValue::UInt64(v) => Ok(ctx.context.i64_type().const_int(*v, false)),
        ConstValue::Unit => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit constants in value position".to_string(),
        )),
    }
}
