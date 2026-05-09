//! Struct literal + field projection emission: `StructInit` and
//! `FieldGet`. The literal path materializes through an entry-block
//! alloca, GEP, store-per-field, then load — matching how every
//! aggregate-shape instruction in this crate threads through LLVM
//! (see also [`crate::emit::enums`]).

use expo_alpha_ir::{IRSymbol, IRType, StructFieldInit};
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

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
        let field_value = lookup(values, field.value)?;
        let field_ptr = build_field_gep(ctx, struct_type, alloca, field.index, ty)?;
        ctx.builder
            .build_store(field_ptr, field_value)
            .map_err(|e| {
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
/// scratch entry-block alloca + GEP + load.
pub(super) fn emit_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    field_type: &IRType,
    struct_symbol: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
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
