//! Constant emission: scalar `ConstValue`s, the heap-payload header
//! shape (`String` / `Binary` / `Bits` literals), and the
//! `LoadConst` cache that materializes pooled aggregate constants
//! through [`emit_ir_constant_aggregate`].

use std::collections::BTreeMap;

use expo_alpha_ir::{ConstValue, EnumPayloadInit, IRConstantValue, IRSymbol};
use inkwell::module::Linkage;
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

use super::enums;

/// Materialize the LLVM SSA value for `LoadConst`, using
/// [`EmitContext::load_const_cache`] so repeat references reuse a
/// single materialization. The constant pool snapshot must have
/// been attached before codegen (see
/// [`crate::ctx::EmitContext::attach_constant_pool`]); a missing
/// pool is a compiler-bug surface.
pub(super) fn emit_load_const<'ctx>(
    ctx: &EmitContext<'ctx>,
    const_id: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if let Some(v) = ctx.load_const_cache.borrow().get(const_id).copied() {
        return Ok(v);
    }
    let pool = ctx.constant_pool.borrow();
    let pool = pool.as_ref().ok_or_else(|| {
        LlvmError::Codegen(
            "LoadConst emitted without ConstantPoolSnapshot — compiler bug \
             (`attach_constant_pool` must precede codegen)"
                .into(),
        )
    })?;
    let entry = pool.get(const_id).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LoadConst references missing pooled constant `{const_id}` — IR seal invariant \
             violated or pool attachment bug",
        ))
    })?;
    let materialized = emit_ir_constant_aggregate(ctx, entry)?;
    ctx.load_const_cache
        .borrow_mut()
        .insert(const_id.clone(), materialized);
    Ok(materialized)
}

/// Lower a scalar `IRInstruction::Const`. `ConstValue::Unit` is
/// machine-representation-less; the merge block of an `if` /
/// `unless` emits a `Const::Unit` so the surrounding expression
/// has a `ValueId` to thread, but in practice nothing downstream
/// consumes it (the if/unless expression is Unit-typed and the
/// function's actual return is the trailing Int / Bool expression
/// that follows the conditional). Returning `None` for Unit lets
/// the caller skip the value-map insert; if some caller does
/// reference the Unit value, the resulting `lookup` miss surfaces
/// as an explicit "undefined SSA value" error rather than a
/// half-shaped i1 / i8 placeholder that papers over a real feature
/// gap.
pub(super) fn emit_const_instruction<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: &ConstValue,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
    if matches!(value, ConstValue::Unit) {
        return Ok(None);
    }
    Ok(Some(emit_const(ctx, value)?))
}

/// Recursively materialize an [`IRConstantValue`] pool entry into a
/// const LLVM SSA value (`StructValue`, enum outer aggregate built
/// the same path as [`enums::emit_enum_construct`], string payload
/// pointer).
fn emit_ir_constant_aggregate<'ctx>(
    ctx: &EmitContext<'ctx>,
    cv: &IRConstantValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match cv {
        IRConstantValue::Primitive(inner) => emit_const(ctx, inner),
        IRConstantValue::EnumVariant { tag, ty } => {
            enums::emit_enum_construct(ctx, &EnumPayloadInit::Unit, *tag, ty, &BTreeMap::new())
        }
        IRConstantValue::Struct { fields, ty } => {
            let struct_type = ctx.layouts.struct_type(ty.mangled());
            let comps: Vec<BasicValueEnum<'ctx>> = fields
                .iter()
                .map(|f| emit_ir_constant_aggregate(ctx, f))
                .collect::<Result<_, _>>()?;
            Ok(struct_type.const_named_struct(&comps).into())
        }
    }
}

fn emit_const<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: &ConstValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match value {
        ConstValue::Binary(bytes) => {
            Ok(emit_const_payload(ctx, bytes, (bytes.len() as u64) * 8, false, "bin").into())
        }
        ConstValue::Bits { bytes, bit_length } => {
            Ok(emit_const_payload(ctx, bytes, *bit_length, false, "bits").into())
        }
        ConstValue::Bool(b) => Ok(ctx
            .context
            .bool_type()
            .const_int(u64::from(*b), false)
            .into()),
        // `const_float` always takes f64; the f32 type narrows on
        // its own (bit-exact since f32 widens losslessly).
        ConstValue::Float32(v) => Ok(ctx.context.f32_type().const_float(f64::from(*v)).into()),
        ConstValue::Float64(v) => Ok(ctx.context.f64_type().const_float(*v).into()),
        ConstValue::Int8(v) => Ok(ctx.context.i8_type().const_int(*v as u64, true).into()),
        ConstValue::Int16(v) => Ok(ctx.context.i16_type().const_int(*v as u64, true).into()),
        ConstValue::Int32(v) => Ok(ctx.context.i32_type().const_int(*v as u64, true).into()),
        ConstValue::Int64(v) => Ok(ctx.context.i64_type().const_int(*v as u64, true).into()),
        ConstValue::String(s) => {
            Ok(emit_const_payload(ctx, s.as_bytes(), (s.len() as u64) * 8, true, "str").into())
        }
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

/// Emit a heap-payload literal as a private constant global with
/// the v1 header layout: `{ i64 bit_length, [N (+1) x i8] bytes }`.
/// Returns a const-GEP to the payload (8 bytes past the header) so
/// the runtime helpers can read `*(payload - 8)` for the bit length
/// without any layout translation.
///
/// `with_nul` adds a trailing `\0` byte to the payload array — used
/// by `String` for libc compat. `Binary` and `Bits` pass `false`:
/// no terminator. `bytes.len()` is the source-byte count; for
/// `Bits` whose `bit_length` is a non-multiple of 8, the producer
/// is responsible for zero-padding the trailing partial byte.
///
/// `prefix` becomes `alpha_<prefix>.<n>` in the LLVM IR — purely
/// cosmetic but helps reading raw IR (`str` / `bin` / `bits`).
fn emit_const_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    bytes: &[u8],
    bit_length: u64,
    with_nul: bool,
    prefix: &str,
) -> PointerValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let array_len = bytes.len() as u32 + if with_nul { 1 } else { 0 };
    let payload_ty = i8_ty.array_type(array_len);
    let header_ty = ctx
        .context
        .struct_type(&[i64_ty.into(), payload_ty.into()], false);
    let bytes_const = ctx.context.const_string(bytes, with_nul);
    let initializer = header_ty.const_named_struct(&[
        i64_ty.const_int(bit_length, false).into(),
        bytes_const.into(),
    ]);
    let symbol = ctx.next_payload_symbol(prefix);
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
