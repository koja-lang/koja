//! Per-instruction dispatch + the const + call helpers it routes
//! to. Operator emission lives in the sibling [`super::ops`] module.

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConcatKind, ConstValue, EnumPayloadInit, IRConstantValue, IRInstruction, IRLocalId, IRSymbol,
    IRType, IRVariantTag, StructFieldInit, ValueId,
};
use inkwell::module::Linkage;
use inkwell::types::StructType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::runtime::{declare_concat_bits_extern, declare_free_extern, declare_malloc_extern};
use crate::types::ir_basic_type;

use super::binary_construct::emit_binary_construct;
use super::{ValueMap, inkwell_err, lookup, ops};

pub(super) fn emit_instruction<'ctx>(
    ctx: &EmitContext<'ctx>,
    instr: &IRInstruction,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    match instr {
        IRInstruction::BinaryConstruct {
            dest,
            layout,
            segments,
        } => {
            let result = emit_binary_construct(ctx, *layout, segments, values)?;
            values.insert(*dest, result);
            Ok(())
        }
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
        IRInstruction::Concat {
            dest,
            kind,
            lhs,
            rhs,
        } => {
            let lhs_value = lookup(values, *lhs)?;
            let rhs_value = lookup(values, *rhs)?;
            let result = emit_concat(ctx, *kind, lhs_value, rhs_value)?;
            values.insert(*dest, result);
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
        IRInstruction::EnumPayloadFieldGet {
            dest,
            field_type,
            payload_index,
            tag,
            ty,
            value,
        } => {
            let base = lookup(values, *value)?;
            let result =
                emit_enum_payload_field_get(ctx, ty, *tag, *payload_index, field_type, base)?;
            values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::EnumTagGet { dest, value, ty } => {
            let base = lookup(values, *value)?;
            let result = emit_enum_tag_get(ctx, ty, base)?;
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
        IRInstruction::DropLocal { local, ty } => emit_drop_local(ctx, *local, ty),
        IRInstruction::LocalDecl { local, ty } => emit_local_decl(ctx, *local, ty),
        IRInstruction::LocalRead { dest, local, ty } => {
            let value = emit_local_read(ctx, *local, ty)?;
            values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite {
            local,
            ownership: _,
            value,
        } => {
            let resolved = lookup(values, *value)?;
            emit_local_write(ctx, *local, resolved)
        }
        IRInstruction::MoveOutLocal { dest, local, ty } => {
            let value = emit_local_read(ctx, *local, ty)?;
            values.insert(*dest, value);
            Ok(())
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

/// Spill `value` to a fresh outer-typed alloca, GEP through the
/// matched variant's `complete` struct to the tag slot, and load
/// it as `i8`. `EnumTagGet` is gated by the typecheck-resolve walk
/// to operate only on enum-typed receivers, so the tag slot always
/// exists at field 0 of the variant's complete struct (every
/// variant's complete struct shares the same `i8` tag prefix —
/// any variant works for the GEP type).
fn emit_enum_tag_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let outer = ctx.layouts.enum_outer_type(ty.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_tag_src"));
    ctx.builder
        .build_store(alloca, value)
        .map_err(|e| inkwell_err(format_args!("build_store for `{ty}` EnumTagGet"), e))?;
    let (complete, _) = ctx
        .layouts
        .enum_variant_types(ty.mangled(), IRVariantTag(0));
    let tag_ptr = ctx
        .builder
        .build_struct_gep(complete, alloca, 0, &format!("{ty}_tag_ptr"))
        .map_err(|e| inkwell_err(format_args!("build_struct_gep for `{ty}` EnumTagGet"), e))?;
    ctx.builder
        .build_load(ctx.context.i8_type(), tag_ptr, &format!("{ty}_tag"))
        .map_err(|e| inkwell_err(format_args!("build_load for `{ty}` EnumTagGet"), e))
}

/// Spill `value` to a fresh outer-typed alloca, GEP through the
/// `tag`-specific complete struct's payload (field 2), then GEP
/// into the variant's payload struct at `payload_index`, and load
/// the field. Caller (the `match` driver) gates this on a
/// successful tag check, so the variant's payload struct is
/// guaranteed to be present and the index in range.
fn emit_enum_payload_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    tag: IRVariantTag,
    payload_index: u32,
    field_type: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let outer = ctx.layouts.enum_outer_type(ty.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_payload_src"));
    ctx.builder.build_store(alloca, value).map_err(|e| {
        inkwell_err(
            format_args!("build_store for `{ty}` EnumPayloadFieldGet"),
            e,
        )
    })?;
    let (complete, payload_struct) = ctx.layouts.enum_variant_types(ty.mangled(), tag);
    let Some(payload_struct) = payload_struct else {
        panic!(
            "alpha LLVM emit: EnumPayloadFieldGet on `{ty}.{tag}` but the variant declares \
             no payload — IR seal invariant violation",
        );
    };
    let payload_ptr = build_payload_gep(ctx, ty, complete, alloca)?;
    let field_ptr = ctx
        .builder
        .build_struct_gep(
            payload_struct,
            payload_ptr,
            payload_index,
            &format!("{ty}_payload_{payload_index}_ptr"),
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for `{ty}` EnumPayloadFieldGet"),
                e,
            )
        })?;
    let field_llvm_type = ir_basic_type(ctx, field_type)?;
    ctx.builder
        .build_load(
            field_llvm_type,
            field_ptr,
            &format!("{ty}_payload_{payload_index}"),
        )
        .map_err(|e| inkwell_err(format_args!("build_load for `{ty}` EnumPayloadFieldGet"), e))
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
    let function = ctx.declared_function(callee).unwrap_or_else(|| {
        panic!(
            "alpha LLVM emit: callee `{}` not registered in the declared-functions \
             index — declaration order or seal violation",
            callee.mangled(),
        )
    });
    let mut arg_values: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
    for arg in args {
        arg_values.push(lookup(values, *arg)?.into());
    }
    let call_site = ctx
        .builder
        .build_call(function, &arg_values, "call")
        .map_err(|e| inkwell_err(format_args!("build_call for `{}`", callee.mangled()), e))?;
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

/// Lower an `IRInstruction::Concat` to its per-kind shape. `String`
/// and `Binary` both byte-align — the common shape is `malloc(8 +
/// total_bytes [+1])` + two `memcpy`s + (String only) trailing
/// `\0`. `Bits` defers to the `__expo_alpha_concat_bits` runtime
/// helper because sub-byte alignment is far cleaner in Rust than
/// LLVM IR.
fn emit_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    kind: ConcatKind,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match kind {
        ConcatKind::Bits => {
            let helper = declare_concat_bits_extern(ctx);
            let result = ctx
                .builder
                .build_call(helper, &[lhs.into(), rhs.into()], "concat_bits")
                .map_err(|e| inkwell_err(format_args!("concat_bits call"), e))?;
            let basic = result.try_as_basic_value().basic().ok_or_else(|| {
                LlvmError::Codegen(
                    "alpha LLVM emit: __expo_alpha_concat_bits returned void; \
                     runtime declaration drift?"
                        .to_string(),
                )
            })?;
            Ok(basic)
        }
        ConcatKind::String | ConcatKind::Binary => {
            emit_byte_aligned_concat(ctx, lhs, rhs, matches!(kind, ConcatKind::String))
        }
    }
}

/// `String` / `Binary` share a single inline shape: load both
/// `i64 bit_length`s from the `payload-8` headers, derive byte
/// counts via `>> 3`, `malloc` the combined block, store the
/// combined `bit_length`, `memcpy` lhs then rhs payloads, and (for
/// `String`) write a trailing `\0`. Returns the new payload pointer.
fn emit_byte_aligned_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    with_nul: bool,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let neg8 = i64_ty.const_int((-8i64) as u64, true);
    let eight = i64_ty.const_int(8, false);
    let three = i64_ty.const_int(3, false);
    let l_ptr = lhs.into_pointer_value();
    let r_ptr = rhs.into_pointer_value();

    let (l_bits, l_bytes) = load_bit_length(ctx, l_ptr, "l", i8_ty, i64_ty, neg8, three)?;
    let (r_bits, r_bytes) = load_bit_length(ctx, r_ptr, "r", i8_ty, i64_ty, neg8, three)?;

    let total_bits = ctx
        .builder
        .build_int_add(l_bits, r_bits, "cat_total_bits")
        .map_err(|e| inkwell_err(format_args!("concat total_bits"), e))?;
    let total_bytes = ctx
        .builder
        .build_int_add(l_bytes, r_bytes, "cat_total_bytes")
        .map_err(|e| inkwell_err(format_args!("concat total_bytes"), e))?;
    let header_size = if with_nul {
        i64_ty.const_int(9, false)
    } else {
        eight
    };
    let alloc_size = ctx
        .builder
        .build_int_add(total_bytes, header_size, "cat_alloc")
        .map_err(|e| inkwell_err(format_args!("concat alloc"), e))?;

    let malloc = declare_malloc_extern(ctx);
    let base = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "cat_base")
        .map_err(|e| inkwell_err(format_args!("concat malloc"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("malloc returned void".to_string()))?
        .into_pointer_value();

    ctx.builder
        .build_store(base, total_bits)
        .map_err(|e| inkwell_err(format_args!("concat store header"), e))?;

    let payload = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, base, &[eight], "cat_payload")
            .map_err(|e| inkwell_err(format_args!("concat payload GEP"), e))?
    };

    ctx.builder
        .build_memcpy(payload, 1, l_ptr, 1, l_bytes)
        .map_err(|e| inkwell_err(format_args!("concat memcpy lhs"), e))?;

    let mid = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, payload, &[l_bytes], "cat_mid")
            .map_err(|e| inkwell_err(format_args!("concat mid GEP"), e))?
    };
    ctx.builder
        .build_memcpy(mid, 1, r_ptr, 1, r_bytes)
        .map_err(|e| inkwell_err(format_args!("concat memcpy rhs"), e))?;

    if with_nul {
        let end = unsafe {
            ctx.builder
                .build_in_bounds_gep(i8_ty, payload, &[total_bytes], "cat_end")
                .map_err(|e| inkwell_err(format_args!("concat end GEP"), e))?
        };
        ctx.builder
            .build_store(end, i8_ty.const_int(0, false))
            .map_err(|e| inkwell_err(format_args!("concat NUL store"), e))?;
    }

    Ok(payload.into())
}

/// Load the `i64 bit_length` header at `payload - 8` plus its
/// derived `bit_length >> 3` byte count. Shared between the lhs /
/// rhs sides of [`emit_byte_aligned_concat`]; `prefix` is just for
/// LLVM SSA-name readability.
fn load_bit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    prefix: &str,
    i8_ty: inkwell::types::IntType<'ctx>,
    i64_ty: inkwell::types::IntType<'ctx>,
    neg8: IntValue<'ctx>,
    three: IntValue<'ctx>,
) -> Result<(IntValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let hdr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg8], &format!("{prefix}_hdr"))
            .map_err(|e| inkwell_err(format_args!("concat header GEP for `{prefix}`"), e))?
    };
    let bits = ctx
        .builder
        .build_load(i64_ty, hdr, &format!("{prefix}_bits"))
        .map_err(|e| inkwell_err(format_args!("concat header load for `{prefix}`"), e))?
        .into_int_value();
    let bytes = ctx
        .builder
        .build_right_shift(bits, three, false, &format!("{prefix}_bytes"))
        .map_err(|e| inkwell_err(format_args!("concat byte count for `{prefix}`"), e))?;
    Ok((bits, bytes))
}

/// `String`, `Binary`, and `Bits` all share the single bit-length-
/// header layout (`[i64 bit_length][payload]` with the SSA pointer
/// at the payload), so a single GEP-by-`-8` + `free` shape covers
/// all three. Non-heap types panic loudly: the lowerer is
/// responsible for never emitting `DropLocal` for stack types
/// (it keys on [`IRType`] in `is_heap_type`).
fn emit_drop_local<'ctx>(
    ctx: &EmitContext<'ctx>,
    local: IRLocalId,
    ty: &IRType,
) -> Result<(), LlvmError> {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => {
            let payload = emit_local_read(ctx, local, ty)?;
            let payload_ptr = payload.into_pointer_value();
            let i8_type = ctx.context.i8_type();
            let i64_type = ctx.context.i64_type();
            let block_base = unsafe {
                ctx.builder.build_gep(
                    i8_type,
                    payload_ptr,
                    &[i64_type.const_int((-8i64) as u64, true)],
                    &format!("{local}.block_base"),
                )
            }
            .map_err(|e| inkwell_err(format_args!("drop block-base GEP for `{local}`"), e))?;
            let free = declare_free_extern(ctx);
            ctx.builder
                .build_call(free, &[block_base.into()], &format!("{local}.free"))
                .map_err(|e| inkwell_err(format_args!("free call for `{local}`"), e))?;
            Ok(())
        }
        _ => panic!(
            "alpha LLVM emit: unsupported `IRInstruction::DropLocal` type {ty:?} for slot `{local}` — \
             extend `emit_drop_local` when more heap types ship",
        ),
    }
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
) -> inkwell::values::PointerValue<'ctx> {
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
