//! `TupleInit` / `TupleGet` emission. Tuples lay out as anonymous
//! LLVM struct types built inline from their element types (see
//! [`crate::types::tuple_struct_type`]), so the alloca + GEP shapes
//! mirror [`super::structs`] without a registered layout to consult.
//! Tuple elements are never cycle-broken `Indirect` slots (only decl
//! fields and enum payloads get stamped), so there is no box / unbox
//! step.

use inkwell::values::BasicValueEnum;
use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::{ir_basic_type, tuple_struct_type};

use super::{ValueMap, lookup};

pub(super) fn emit_tuple_init<'ctx>(
    ctx: &EmitContext<'ctx>,
    elements: &[ValueId],
    ty: &[IRType],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let tuple_type = tuple_struct_type(ctx, ty)?;
    let alloca = ctx.build_entry_alloca(tuple_type, "tuple_tmp");
    for (index, element) in elements.iter().enumerate() {
        let value = lookup(values, *element)?;
        let label = format!("tuple_elem_{index}");
        let element_ptr = ctx
            .builder
            .build_struct_gep(tuple_type, alloca, index as u32, &label)
            .or_ice()?;
        ctx.builder.build_store(element_ptr, value).or_ice()?;
    }
    ctx.builder.build_load(tuple_type, alloca, "tuple").or_ice()
}

/// Project one element out of a tuple-typed SSA value. The base
/// value's own LLVM struct type drives the GEP, since the
/// instruction only carries the projected element's type.
pub(super) fn emit_tuple_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    index: u32,
    element_type: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let tuple_value = base.into_struct_value();
    let tuple_type = tuple_value.get_type();
    let alloca = ctx.build_entry_alloca(tuple_type, "tuple_get_tmp");
    ctx.builder.build_store(alloca, tuple_value).or_ice()?;
    let label = format!("tuple_elem_{index}");
    let element_ptr = ctx
        .builder
        .build_struct_gep(tuple_type, alloca, index, &label)
        .or_ice()?;
    let element_llvm_type = ir_basic_type(ctx, element_type)?;
    ctx.builder
        .build_load(element_llvm_type, element_ptr, &label)
        .or_ice()
}
