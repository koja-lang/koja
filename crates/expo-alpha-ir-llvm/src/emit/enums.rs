//! Enum literal + projection emission: `EnumConstruct`, `EnumTagGet`,
//! `EnumPayloadFieldGet`. Every shape spills the SSA value through
//! an entry-block alloca, GEPs through the variant's complete
//! struct (tag at field 0, payload at field 2), and reads or
//! writes whichever slot the instruction targets.

use expo_alpha_ir::{EnumPayloadInit, IRSymbol, IRType, IRVariantTag, StructFieldInit, ValueId};
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

use super::{ValueMap, inkwell_err, lookup};

/// Materialize an enum-variant literal: alloca the outer enum
/// blob, GEP through the per-variant complete struct to write the
/// `i8` tag, GEP further into the payload struct (when present) to
/// write each payload field, then load the populated outer value
/// out as the SSA result. Per-shape payload writes are split into
/// [`emit_tuple_payload`] / [`emit_struct_payload`] so each arm
/// stays small and the shape match here is one line.
pub(super) fn emit_enum_construct<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &EnumPayloadInit,
    tag: IRVariantTag,
    ty: &IRSymbol,
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
pub(super) fn emit_enum_tag_get<'ctx>(
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
pub(super) fn emit_enum_payload_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    field_type: &IRType,
    payload_index: u32,
    tag: IRVariantTag,
    ty: &IRSymbol,
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
