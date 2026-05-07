//! Per-instruction dispatch + the const + call helpers it routes
//! to. Operator emission lives in the sibling [`super::ops`] module.

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConstValue, EnumPayloadInit, IRConstantValue, IRInstruction, IRLocalId, IRSymbol, IRType,
    IRVariantTag, StructFieldInit, ValueId,
};
use inkwell::module::Linkage;
use inkwell::types::StructType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

use super::{ValueMap, inkwell_err, lookup, ops};

pub(super) fn emit_instruction<'ctx>(
    ctx: &EmitContext<'ctx>,
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
            if let Some(result) = emit_call(ctx, callee, args, values)? {
                values.insert(*dest, result);
            }
            Ok(())
        }
        IRInstruction::EnumConstruct {
            dest,
            payload,
            tag,
            ty,
        } => {
            let result = emit_enum_construct(ctx, ty, *tag, payload, values)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::LoadConst {
            dest,
            const_id,
            ty: _,
        } => {
            let llvm_val = {
                let cache_hit = ctx.load_const_cache.borrow().get(const_id).copied();
                if let Some(v) = cache_hit {
                    v
                } else {
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
                    materialized
                }
            };
            values.insert(*dest, llvm_val);
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
        IRInstruction::FieldGet {
            base,
            dest,
            field_index,
            field_type,
            struct_symbol,
        } => {
            let base_value = lookup(values, *base)?;
            let struct_type = ctx.layouts.struct_type(struct_symbol.mangled());
            let result = emit_field_get(ctx, struct_type, base_value, *field_index, field_type)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::LocalDecl { local, ty } => emit_local_decl(ctx, *local, ty),
        IRInstruction::LocalRead { dest, local, ty } => {
            let value = emit_local_read(ctx, *local, ty)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite { local, value } => {
            let resolved = lookup(values, *value)?;
            emit_local_write(ctx, *local, resolved)
        }
        IRInstruction::StructInit { dest, fields, ty } => {
            let struct_type = ctx.layouts.struct_type(ty.mangled());
            let result = emit_struct_init(ctx, struct_type, ty, fields, values)?;
            values.insert(*dest, result);
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

/// Materialize a struct literal: hoist a scratch alloca to the
/// entry block, store each field through a `getelementptr`, then
/// load the populated struct out as the instruction's SSA value.
fn emit_struct_init<'ctx>(
    ctx: &EmitContext<'ctx>,
    struct_type: StructType<'ctx>,
    symbol: &IRSymbol,
    fields: &[StructFieldInit],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let alloca = ctx.build_entry_alloca(struct_type, &format!("{symbol}_tmp"));
    for field in fields {
        let field_value = lookup(values, field.value)?;
        let field_ptr = build_field_gep(ctx, struct_type, alloca, field.index, symbol)?;
        ctx.builder
            .build_store(field_ptr, field_value)
            .map_err(|e| {
                inkwell_err(
                    format_args!("build_store for `{symbol}` field #{}", field.index),
                    e,
                )
            })?;
    }
    ctx.builder
        .build_load(struct_type, alloca, symbol.mangled())
        .map_err(|e| {
            inkwell_err(
                format_args!("build_load for `{symbol}` after StructInit"),
                e,
            )
        })
}

/// Project a single field out of a struct-typed SSA value via a
/// scratch entry-block alloca + GEP + load.
fn emit_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    struct_type: StructType<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    field_type: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let struct_value = base.into_struct_value();
    let alloca = ctx.build_entry_alloca(struct_type, "field_tmp");
    ctx.builder
        .build_store(alloca, struct_value)
        .map_err(|e| inkwell_err("build_store for FieldGet", e))?;
    let label = format!("field_{field_index}");
    let field_ptr = ctx
        .builder
        .build_struct_gep(struct_type, alloca, field_index, &label)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for FieldGet field #{field_index}"),
                e,
            )
        })?;
    let field_llvm_type = ir_basic_type(ctx, field_type)?;
    ctx.builder
        .build_load(field_llvm_type, field_ptr, &label)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_load for FieldGet field #{field_index}"),
                e,
            )
        })
}

/// Materialize an enum-variant literal: alloca the outer enum
/// blob, GEP through the per-variant complete struct to write the
/// `i8` tag, GEP further into the payload struct (when present) to
/// write each payload field, then load the populated outer value
/// out as the SSA result. Per-shape payload writes are split into
/// [`emit_tuple_payload`] / [`emit_struct_payload`] so each arm
/// stays small and the shape match here is one line.
fn emit_enum_construct<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    tag: IRVariantTag,
    payload: &EnumPayloadInit,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let outer = ctx.layouts.enum_outer_type(ty.mangled());
    let (complete, payload_type) = ctx.layouts.enum_variant_types(ty.mangled(), tag);
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_tmp"));
    write_variant_tag(ctx, ty, tag, complete, alloca)?;
    if let (Some(payload_struct), payload_init) = (payload_type, payload) {
        let payload_ptr = build_payload_gep(ctx, ty, complete, alloca)?;
        match payload_init {
            EnumPayloadInit::Tuple(operands) => {
                emit_tuple_payload(ctx, ty, payload_struct, payload_ptr, operands, values)?;
            }
            EnumPayloadInit::Struct(fields) => {
                emit_struct_payload(ctx, ty, payload_struct, payload_ptr, fields, values)?;
            }
            EnumPayloadInit::Unit => {
                panic!(
                    "alpha LLVM emit: enum `{ty}` variant has a payload type but the \
                     instruction's payload is Unit — IR seal invariant violation",
                );
            }
        }
    }
    ctx.builder
        .build_load(outer, alloca, ty.mangled())
        .map_err(|e| inkwell_err(format_args!("build_load for `{ty}` after EnumConstruct"), e))
}

fn write_variant_tag<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    tag: IRVariantTag,
    complete: StructType<'ctx>,
    alloca: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    let tag_ptr = ctx
        .builder
        .build_struct_gep(complete, alloca, 0, &format!("{ty}_tag"))
        .map_err(|e| inkwell_err(format_args!("build_struct_gep for `{ty}` tag"), e))?;
    let tag_value = ctx.context.i8_type().const_int(u64::from(tag.0), false);
    ctx.builder
        .build_store(tag_ptr, tag_value)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_store for `{ty}` tag"), e))
}

fn build_payload_gep<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    complete: StructType<'ctx>,
    alloca: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_struct_gep(complete, alloca, 2, &format!("{ty}_payload"))
        .map_err(|e| inkwell_err(format_args!("build_struct_gep for `{ty}` payload"), e))
}

fn emit_tuple_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    operands: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    for (index, operand) in operands.iter().enumerate() {
        let value = lookup(values, *operand)?;
        let field_ptr = ctx
            .builder
            .build_struct_gep(
                payload_type,
                payload_ptr,
                index as u32,
                &format!("{ty}_tuple_{index}"),
            )
            .map_err(|e| {
                inkwell_err(
                    format_args!("build_struct_gep for `{ty}` tuple element #{index}"),
                    e,
                )
            })?;
        ctx.builder.build_store(field_ptr, value).map_err(|e| {
            inkwell_err(
                format_args!("build_store for `{ty}` tuple element #{index}"),
                e,
            )
        })?;
    }
    Ok(())
}

fn emit_struct_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    fields: &[StructFieldInit],
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    for field in fields {
        let value = lookup(values, field.value)?;
        let field_ptr = ctx
            .builder
            .build_struct_gep(
                payload_type,
                payload_ptr,
                field.index,
                &format!("{ty}_field_{}", field.index),
            )
            .map_err(|e| {
                inkwell_err(
                    format_args!("build_struct_gep for `{ty}` struct field #{}", field.index),
                    e,
                )
            })?;
        ctx.builder.build_store(field_ptr, value).map_err(|e| {
            inkwell_err(
                format_args!("build_store for `{ty}` struct field #{}", field.index),
                e,
            )
        })?;
    }
    Ok(())
}

fn build_field_gep<'ctx>(
    ctx: &EmitContext<'ctx>,
    struct_type: StructType<'ctx>,
    base_ptr: PointerValue<'ctx>,
    field_index: u32,
    symbol: &IRSymbol,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let label = format!("{symbol}_field_{field_index}");
    ctx.builder
        .build_struct_gep(struct_type, base_ptr, field_index, &label)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{symbol}` field #{field_index}"),
                e,
            )
        })
}

/// Call the function registered on `ctx.module` under the callee's
/// mangled symbol. Returns `None` for `Unit`-returning callees (LLVM
/// `void` calls); the caller skips the value-map insert in that case.
fn emit_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    callee: &IRSymbol,
    args: &[ValueId],
    values: &ValueMap<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
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
        .map_err(|e| inkwell_err(format_args!("build_call for `{mangled}`"), e))?;
    Ok(call_site.try_as_basic_value().basic())
}

/// Materialize a `LocalDecl` as an entry-block `alloca`, stashed on
/// the [`EmitContext`] keyed by [`IRLocalId`] for later `load` / `store`.
fn emit_local_decl<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    let llvm_ty = ir_basic_type(ctx, ty)?;
    let name = local.to_string();
    let slot = ctx.build_entry_alloca(llvm_ty, &name);
    ctx.register_local_slot(local, slot);
    Ok(())
}

/// Lower a `LocalRead` to an LLVM `load`. Pointer comes from the
/// per-function slot table; load type comes from the IR's static
/// type slot.
fn emit_local_read<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let slot = ctx.local_slot(local);
    let llvm_ty = ir_basic_type(ctx, ty)?;
    ctx.builder
        .build_load(llvm_ty, slot, &local.to_string())
        .map_err(|e| inkwell_err(format_args!("build_load for `{local}`"), e))
}

/// Lower a `LocalWrite` to an LLVM `store` into the slot table's
/// pointer for `local`.
fn emit_local_write<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let slot = ctx.local_slot(local);
    ctx.builder
        .build_store(slot, value)
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_store for `{local}`"), e))
}

/// Recursively materialize an [`IRConstantValue`] pool entry into a
/// const LLVM SSA value (`StructValue`, enum outer aggregate built
/// the same path as [`emit_enum_construct`], string payload pointer).
fn emit_ir_constant_aggregate<'ctx>(
    ctx: &EmitContext<'ctx>,
    cv: &IRConstantValue,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match cv {
        IRConstantValue::Primitive(inner) => emit_const(ctx, inner),
        IRConstantValue::EnumVariant { tag, ty } => {
            emit_enum_construct(ctx, ty, *tag, &EnumPayloadInit::Unit, &BTreeMap::new())
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
    ctx: &EmitContext<'ctx>,
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
