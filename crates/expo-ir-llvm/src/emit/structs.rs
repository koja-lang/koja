//! Struct literal + field projection emission: `StructInit` and
//! `FieldGet`. The literal path materializes through an entry-block
//! alloca, GEP, store-per-field, then load — matching how every
//! aggregate-shape instruction in this crate threads through LLVM
//! (see also [`crate::emit::enums`]).

use expo_ir::{IRSymbol, IRType, StructFieldInit};
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

use super::indirect::{emit_box_value, emit_unbox_value};
use super::{ValueMap, inkwell_err, lookup};

/// Materialize a struct literal: hoist a scratch alloca to the
/// entry block, store each field through a `getelementptr`, then
/// load the populated struct out as the instruction's SSA value.
pub(super) fn emit_struct_init<'ctx>(
    ctx: &EmitContext<'ctx>,
    fields: &[StructFieldInit],
    ty: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let struct_type = ctx.layouts.struct_type(ty.mangled());
    let alloca = ctx.build_entry_alloca(struct_type, &format!("{ty}_tmp"));
    for field in fields {
        let raw_value = lookup(values, field.value)?;
        let declared_ty = ctx.layouts.struct_field_ir_type(ty, field.index as usize);
        let stored = match &declared_ty {
            IRType::Indirect(inner) => emit_box_value(
                ctx,
                inner,
                raw_value,
                &format!("{ty}_field_{}_box", field.index),
            )?,
            _ => raw_value,
        };
        let field_ptr = build_field_gep(ctx, struct_type, alloca, field.index, ty)?;
        ctx.builder.build_store(field_ptr, stored).map_err(|e| {
            inkwell_err(
                format_args!("build_store for `{ty}` field #{}", field.index),
                e,
            )
        })?;
    }
    ctx.builder
        .build_load(struct_type, alloca, ty.mangled())
        .map_err(|e| inkwell_err(format_args!("build_load for `{ty}` after StructInit"), e))
}

/// Project a single field out of a struct-typed SSA value via a
/// scratch entry-block alloca + GEP + load. The `field_type` passed
/// by the instruction is the unboxed view; the decl's recorded type
/// drives the actual load shape so cycle-broken `Indirect(_)` slots
/// load a `ptr` then unbox.
pub(super) fn emit_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    field_type: &IRType,
    struct_symbol: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let _ = field_type;
    let declared_ty = ctx
        .layouts
        .struct_field_ir_type(struct_symbol, field_index as usize);
    let struct_type = ctx.layouts.struct_type(struct_symbol.mangled());
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
    let field_llvm_type = ir_basic_type(ctx, &declared_ty)?;
    let loaded = ctx
        .builder
        .build_load(field_llvm_type, field_ptr, &label)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_load for FieldGet field #{field_index}"),
                e,
            )
        })?;
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

/// Produce a struct-typed SSA value identical to `base` except the
/// field at `field_index` is replaced by `value`. Same alloca + GEP
/// pattern as [`emit_field_get`]: copy the base struct into a scratch
/// alloca, GEP-store the new field over its slot, then reload the
/// whole struct as the instruction's SSA destination.
pub(super) fn emit_field_set<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    field_type: &IRType,
    struct_symbol: &IRSymbol,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let _ = field_type;
    let declared_ty = ctx
        .layouts
        .struct_field_ir_type(struct_symbol, field_index as usize);
    let stored = match &declared_ty {
        IRType::Indirect(inner) => emit_box_value(
            ctx,
            inner,
            value,
            &format!("{struct_symbol}_field_{field_index}_set_box"),
        )?,
        _ => value,
    };
    let struct_type = ctx.layouts.struct_type(struct_symbol.mangled());
    let struct_value = base.into_struct_value();
    let alloca = ctx.build_entry_alloca(struct_type, "field_set_tmp");
    ctx.builder
        .build_store(alloca, struct_value)
        .map_err(|e| inkwell_err("build_store for FieldSet base", e))?;
    let label = format!("field_set_{field_index}");
    let field_ptr = ctx
        .builder
        .build_struct_gep(struct_type, alloca, field_index, &label)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_struct_gep for FieldSet field #{field_index}"),
                e,
            )
        })?;
    ctx.builder.build_store(field_ptr, stored).map_err(|e| {
        inkwell_err(
            format_args!("build_store for FieldSet field #{field_index}"),
            e,
        )
    })?;
    ctx.builder
        .build_load(struct_type, alloca, struct_symbol.mangled())
        .map_err(|e| inkwell_err("build_load for FieldSet result", e))
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
