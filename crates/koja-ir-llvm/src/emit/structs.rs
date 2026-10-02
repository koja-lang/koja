//! Struct literal and field projection emission for `StructInit`,
//! `FieldGet`, and `FieldSet`. Struct values are SSA aggregates, so
//! the literal builds up from `undef` with one `insertvalue` per
//! field and the projections use `extractvalue` / `insertvalue`
//! directly. Enums go through memory instead (see
//! [`crate::emit::enums`]) because their outer blob is reinterpreted
//! per variant.

use inkwell::values::BasicValueEnum;
use koja_ir::{IRSymbol, IRType, StructFieldInit};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};

use super::indirect::{emit_box_value, emit_unbox_value};
use super::{ValueMap, lookup};

/// Materialize a struct literal. Start from `undef` and
/// `insertvalue` each field in IR order, naming the final value
/// after the struct.
pub(super) fn emit_struct_init<'ctx>(
    ctx: &EmitContext<'ctx>,
    fields: &[StructFieldInit],
    ty: &IRSymbol,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let struct_type = ctx.layouts.struct_type(ty.mangled());
    let mut aggregate = struct_type.get_undef();
    for field in fields {
        let raw_value = lookup(values, field.value);
        let declared_ty = ctx.layouts.struct_field_ir_type(ty, field.index as usize);
        let stored = match &declared_ty {
            IRType::Indirect(inner) => box_or_pass_through(
                ctx,
                inner,
                raw_value,
                &format!("{ty}_field_{}_box", field.index),
            )?,
            _ => raw_value,
        };
        aggregate = ctx
            .builder
            .build_insert_value(
                aggregate,
                stored,
                field.index,
                &format!("{ty}_field_{}", field.index),
            )
            .or_ice()?
            .into_struct_value();
    }
    Ok(aggregate.into())
}

/// Fill an `Indirect` slot. Box an unboxed inner value, or store an
/// already-boxed pointer directly (clone glue passes the shared box
/// through). The two are distinguishable by LLVM value kind, since a
/// box's inner type is always an aggregate (cycle breaking only
/// stamps struct / enum / tuple / union slots), never pointer-shaped.
pub(super) fn box_or_pass_through<'ctx>(
    ctx: &EmitContext<'ctx>,
    inner: &IRType,
    value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if value.is_pointer_value() {
        return Ok(value);
    }
    emit_box_value(ctx, inner, value, label)
}

/// Project a single field out of a struct-typed SSA value with
/// `extractvalue`. A cycle-broken `Indirect(_)` slot yields a `ptr`,
/// which is then unboxed for the usual unboxed instruction view, or
/// handed through raw when the instruction's `field_type` is itself
/// `Indirect` (glue and overwrite sites project the box to `rc++` /
/// release it).
pub(super) fn emit_field_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    field_type: &IRType,
    struct_symbol: &IRSymbol,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let declared_ty = ctx
        .layouts
        .struct_field_ir_type(struct_symbol, field_index as usize);
    let label = format!("field_{field_index}");
    let extracted = ctx
        .builder
        .build_extract_value(base.into_struct_value(), field_index, &label)
        .or_ice()?;
    if let IRType::Indirect(inner) = &declared_ty
        && !matches!(field_type, IRType::Indirect(_))
    {
        return emit_unbox_value(
            ctx,
            inner,
            extracted.into_pointer_value(),
            &format!("{label}_unbox"),
        );
    }
    Ok(extracted)
}

/// Produce a struct-typed SSA value identical to `base` except the
/// field at `field_index` is replaced by `value`, with one
/// `insertvalue` over the base aggregate.
pub(super) fn emit_field_set<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    field_index: u32,
    struct_symbol: &IRSymbol,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
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
    ctx.builder
        .build_insert_value(
            base.into_struct_value(),
            stored,
            field_index,
            struct_symbol.mangled(),
        )
        .or_ice()
        .map(|v| v.into_struct_value().into())
}
