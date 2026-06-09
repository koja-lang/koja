//! Enum literal + projection emission: `EnumConstruct`, `EnumTagGet`,
//! `EnumPayloadFieldGet`. Every shape spills the SSA value through
//! an entry-block alloca, GEPs through the variant's complete
//! struct (tag at field 0, payload at field 2), and reads or
//! writes whichever slot the instruction targets.
//!
//! The lower-level [`build_enum_value`] is also `pub(crate)` so
//! intrinsic emitters (`intrinsics/list.rs`, `intrinsics/string.rs`,
//! `intrinsics/map.rs`) that need to mint an `Option::Some(_)` /
//! `Option::None` / `Result::Ok(_)` / etc. go through the same
//! layout-aware path the instruction emitter uses, instead of
//! GEPing raw indices into an assumed-flat outer struct.

use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, PointerValue};
use koja_ir::{EnumPayloadInit, IRSymbol, IRType, IRVariantPayload, IRVariantTag, StructFieldInit};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::ir_basic_type;

use super::indirect::{emit_box_value, emit_unbox_value};
use super::{ValueMap, lookup};

/// Materialize an enum-variant literal: resolve `payload` operands
/// against `values`, then delegate to [`build_enum_value`] for the
/// alloca/GEP/load dance. Tuple operands keep their declaration
/// order; struct operands are re-keyed into field-index order so
/// the helper sees one positional shape.
pub(super) fn emit_enum_construct<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &EnumPayloadInit,
    tag: IRVariantTag,
    ty: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let payload_values: Vec<BasicValueEnum<'ctx>> = match payload {
        EnumPayloadInit::Struct(fields) => resolve_struct_payload(fields, values)?,
        EnumPayloadInit::Tuple(operands) => operands
            .iter()
            .map(|v| lookup(values, *v))
            .collect::<Result<_, _>>()?,
        EnumPayloadInit::Unit => Vec::new(),
    };
    build_enum_value(ctx, ty, tag, &payload_values)
}

/// Construct a variant value of enum `ty` at `tag` whose payload
/// fields are positionally `payload_values`. The variant's payload
/// must either be absent (`payload_values` empty) or be a tuple /
/// struct whose arity matches `payload_values.len()`. The helper
/// allocas the outer chunk-array struct, GEPs through the variant's
/// `complete` (tag at field 0, payload at field 2 — see
/// [`crate::layout::enums`]), writes each payload field, and loads
/// the populated outer back out.
pub(crate) fn build_enum_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    tag: IRVariantTag,
    payload_values: &[BasicValueEnum<'ctx>],
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let outer = ctx.enum_outer_type(ty.mangled());
    let (complete, payload_type) = ctx.layouts.enum_variant_types(ty.mangled(), tag);
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_tmp"));
    write_variant_tag(ctx, ty, tag, complete, alloca)?;
    let boxed_values: Vec<BasicValueEnum<'ctx>> = if payload_values.is_empty() {
        Vec::new()
    } else {
        box_payload_indirects(ctx, ty, tag, payload_values)?
    };
    match (payload_type, boxed_values.is_empty()) {
        (Some(payload_struct), false) => {
            let payload_ptr = build_payload_gep(ctx, ty, complete, alloca)?;
            write_payload_fields(ctx, ty, payload_struct, payload_ptr, &boxed_values)?;
        }
        (None, true) => {}
        (Some(_), true) => panic!(
            "LLVM emit: enum `{ty}` variant at tag {tag} declares a payload but \
             build_enum_value was called with no payload values — caller mismatch",
        ),
        (None, false) => panic!(
            "LLVM emit: enum `{ty}` variant at tag {tag} is Unit but build_enum_value \
             was called with {} payload value(s) — caller mismatch",
            boxed_values.len(),
        ),
    }
    ctx.builder.build_load(outer, alloca, ty.mangled()).or_ice()
}

fn resolve_struct_payload<'ctx>(
    fields: &[StructFieldInit],
    values: &ValueMap<'ctx>,
) -> Result<Vec<BasicValueEnum<'ctx>>, LlvmError> {
    let arity = fields
        .iter()
        .map(|f| f.index as usize + 1)
        .max()
        .unwrap_or(0);
    let mut slots: Vec<Option<BasicValueEnum<'ctx>>> = vec![None; arity];
    for field in fields {
        let value = lookup(values, field.value)?;
        let slot = slots.get_mut(field.index as usize).unwrap_or_else(|| {
            panic!(
                "LLVM emit: struct payload field index {} out of bounds (arity {arity})",
                field.index,
            )
        });
        if slot.replace(value).is_some() {
            panic!(
                "LLVM emit: struct payload field {} written twice — IR seal \
                 invariant violation",
                field.index,
            );
        }
    }
    slots
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            slot.ok_or_else(|| {
                LlvmError::Codegen(format!(
                    "struct payload missing field at index {i} — IR seal invariant violation",
                ))
            })
        })
        .collect()
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
    let outer = ctx.enum_outer_type(ty.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_tag_src"));
    ctx.builder.build_store(alloca, value).or_ice()?;
    let (complete, _) = ctx
        .layouts
        .enum_variant_types(ty.mangled(), IRVariantTag(0));
    let tag_ptr = ctx
        .builder
        .build_struct_gep(complete, alloca, 0, &format!("{ty}_tag_ptr"))
        .or_ice()?;
    ctx.builder
        .build_load(ctx.context.i8_type(), tag_ptr, &format!("{ty}_tag"))
        .or_ice()
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
    let outer = ctx.enum_outer_type(ty.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{ty}_payload_src"));
    ctx.builder.build_store(alloca, value).or_ice()?;
    let _ = field_type;
    let declared_payload = ctx.layouts.enum_variant_payload(ty, tag);
    let declared_ty = declared_slot_type(&declared_payload, payload_index).unwrap_or_else(|| {
        panic!(
            "LLVM emit: EnumPayloadFieldGet on `{ty}.{tag}` payload index \
             {payload_index} out of range — IR seal invariant violation",
        )
    });
    let (complete, payload_struct) = ctx.layouts.enum_variant_types(ty.mangled(), tag);
    let Some(payload_struct) = payload_struct else {
        panic!(
            "LLVM emit: EnumPayloadFieldGet on `{ty}.{tag}` but the variant declares \
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
        .or_ice()?;
    let field_llvm_type = ir_basic_type(ctx, &declared_ty)?;
    let label = format!("{ty}_payload_{payload_index}");
    let loaded = ctx
        .builder
        .build_load(field_llvm_type, field_ptr, &label)
        .or_ice()?;
    if let IRType::Indirect(inner) = &declared_ty {
        return emit_unbox_value(
            ctx,
            inner,
            loaded.into_pointer_value(),
            &format!("{label}_unbox"),
        );
    }
    Ok(loaded)
}

/// Look up the declared IR type of `payload`'s slot at `index`.
/// Mirrors the `IRVariantPayload` shape: tuples index directly,
/// struct payloads index into the field vec.
fn declared_slot_type(payload: &IRVariantPayload, index: u32) -> Option<IRType> {
    match payload {
        IRVariantPayload::Tuple(types) => types.get(index as usize).cloned(),
        IRVariantPayload::Struct(fields) => fields.get(index as usize).map(|f| f.ir_type.clone()),
        IRVariantPayload::Unit => None,
    }
}

/// Walk the variant's declared payload slot types; for each
/// [`IRType::Indirect`] slot, box the matching incoming value via
/// [`emit_box_value`] so the store hits a ptr slot rather than the
/// raw value.
fn box_payload_indirects<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    tag: IRVariantTag,
    payload_values: &[BasicValueEnum<'ctx>],
) -> Result<Vec<BasicValueEnum<'ctx>>, LlvmError> {
    let payload = ctx.layouts.enum_variant_payload(ty, tag);
    let slot_types: Vec<IRType> = match &payload {
        IRVariantPayload::Tuple(types) => types.clone(),
        IRVariantPayload::Struct(fields) => fields.iter().map(|f| f.ir_type.clone()).collect(),
        IRVariantPayload::Unit => Vec::new(),
    };
    let mut out = Vec::with_capacity(payload_values.len());
    for (idx, value) in payload_values.iter().enumerate() {
        let stored = match slot_types.get(idx) {
            Some(IRType::Indirect(inner)) => {
                emit_box_value(ctx, inner, *value, &format!("{ty}_payload_{idx}_box"))?
            }
            _ => *value,
        };
        out.push(stored);
    }
    Ok(out)
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
        .or_ice()?;
    let tag_value = ctx.context.i8_type().const_int(u64::from(tag.0), false);
    ctx.builder
        .build_store(tag_ptr, tag_value)
        .or_ice()
        .map(|_| ())
}

fn build_payload_gep<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    complete: StructType<'ctx>,
    alloca: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_struct_gep(complete, alloca, 2, &format!("{ty}_payload"))
        .or_ice()
}

fn write_payload_fields<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRSymbol,
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    values: &[BasicValueEnum<'ctx>],
) -> Result<(), LlvmError> {
    for (index, value) in values.iter().enumerate() {
        let field_ptr = ctx
            .builder
            .build_struct_gep(
                payload_type,
                payload_ptr,
                index as u32,
                &format!("{ty}_payload_{index}"),
            )
            .or_ice()?;
        ctx.builder.build_store(field_ptr, *value).or_ice()?;
    }
    Ok(())
}
