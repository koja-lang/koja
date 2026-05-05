//! Per-instruction dispatch + the const + call helpers it routes
//! to. Operator emission lives in the sibling [`super::ops`] module.

use expo_alpha_ir::{ConstValue, IRInstruction, IRSymbol, ValueId};
use inkwell::module::Linkage;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::ctx::EmitCtx;
use crate::error::LlvmError;

use super::{ValueMap, lookup, lookup_int, ops};

pub(super) fn emit_instruction<'ctx>(
    ctx: &EmitCtx<'ctx>,
    instr: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match instr {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup_int(values, *lhs)?;
            let rhs_value = lookup_int(values, *rhs)?;
            let result = ops::emit_binary_op(ctx, *op, lhs_value, rhs_value)?;
            values.insert(*dest, result.into());
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
            let operand_value = lookup_int(values, *operand)?;
            let result = ops::emit_unary_op(ctx, *op, operand_value)?;
            values.insert(*dest, result.into());
            Ok(())
        }
    }
}

/// Emit a call to the helper function registered on `ctx.module`
/// under the callee's mangled symbol. Every non-entry function is
/// declared before any body emission and the IR seal pass guarantees
/// every `IRInstruction::Call::callee` resolves to a registered
/// function — so a miss here is a compiler bug, not a feature gap.
/// `Unit` returns are rejected upstream so we always have a basic
/// value to extract.
fn emit_call<'ctx>(
    ctx: &EmitCtx<'ctx>,
    callee: &IRSymbol,
    args: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
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
    call_site.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "alpha LLVM does not yet emit Unit-returning calls (callee `{mangled}`)",
        ))
    })
}

fn emit_const<'ctx>(
    ctx: &EmitCtx<'ctx>,
    value: &ConstValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match value {
        ConstValue::Bool(b) => Ok(ctx
            .context
            .bool_type()
            .const_int(u64::from(*b), false)
            .into()),
        ConstValue::Int8(v) => Ok(ctx.context.i8_type().const_int(*v as u64, true).into()),
        ConstValue::Int16(v) => Ok(ctx.context.i16_type().const_int(*v as u64, true).into()),
        ConstValue::Int32(v) => Ok(ctx.context.i32_type().const_int(*v as u64, true).into()),
        ConstValue::Int64(v) => Ok(ctx.context.i64_type().const_int(*v as u64, true).into()),
        ConstValue::String(s) => Ok(emit_const_string(ctx, s.as_bytes()).into()),
        ConstValue::UInt8(v) => Ok(ctx.context.i8_type().const_int(u64::from(*v), false).into()),
        ConstValue::UInt16(v) => Ok(ctx
            .context
            .i16_type()
            .const_int(u64::from(*v), false)
            .into()),
        ConstValue::UInt32(v) => Ok(ctx
            .context
            .i32_type()
            .const_int(u64::from(*v), false)
            .into()),
        ConstValue::UInt64(v) => Ok(ctx.context.i64_type().const_int(*v, false).into()),
        ConstValue::Unit => Err(LlvmError::Codegen(
            "alpha LLVM does not yet emit Unit constants in value position".to_string(),
        )),
    }
}

/// Emit a string literal as a private constant global with the v1
/// header layout: `{ i64 bit_length, [N+1 x i8] bytes\00 }`. Returns a
/// const-GEP to the payload (8 bytes past the header), matching
/// `expo-codegen`'s `create_string_global` byte-for-byte so the
/// runtime printer + future `String.to_cstring()` intrinsic can read
/// `*(payload - 8)` for the bit-length without any layout
/// translation.
fn emit_const_string<'ctx>(
    ctx: &EmitCtx<'ctx>,
    bytes: &[u8],
) -> inkwell::values::PointerValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let payload_ty = i8_ty.array_type(bytes.len() as u32 + 1);
    let header_ty = ctx
        .context
        .struct_type(&[i64_ty.into(), payload_ty.into()], false);
    let bytes_const = ctx.context.const_string(bytes, true);
    let bit_length = (bytes.len() as u64) * 8;
    let initializer = header_ty.const_named_struct(&[
        i64_ty.const_int(bit_length, false).into(),
        bytes_const.into(),
    ]);
    let symbol = ctx.next_string_symbol();
    let global = ctx.module.add_global(header_ty, None, &symbol);
    global.set_initializer(&initializer);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    unsafe {
        global
            .as_pointer_value()
            .const_gep(i8_ty, &[i64_ty.const_int(8, false)])
    }
}
