//! `TupleInit` / `TupleGet` emission. Tuples lay out as anonymous
//! LLVM struct types built inline from their element types (see
//! [`crate::types::tuple_struct_type`]), so the `insertvalue` /
//! `extractvalue` shapes mirror [`super::structs`] without a
//! registered layout to consult. Tuple elements are never
//! cycle-broken `Indirect` slots (only decl fields and enum payloads
//! get stamped), so there is no box / unbox step.

use inkwell::values::BasicValueEnum;
use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::tuple_struct_type;

use super::{ValueMap, lookup};

pub(super) fn emit_tuple_init<'ctx>(
    ctx: &EmitContext<'ctx>,
    elements: &[ValueId],
    ty: &[IRType],
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let mut aggregate = tuple_struct_type(ctx, ty)?.get_undef();
    for (index, element) in elements.iter().enumerate() {
        let value = lookup(values, *element);
        aggregate = ctx
            .builder
            .build_insert_value(
                aggregate,
                value,
                index as u32,
                &format!("tuple_elem_{index}"),
            )
            .or_ice()?
            .into_struct_value();
    }
    Ok(aggregate.into())
}

/// Project one element out of a tuple-typed SSA value with
/// `extractvalue`. The element type comes from the base value's own
/// LLVM struct type.
pub(super) fn emit_tuple_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: BasicValueEnum<'ctx>,
    index: u32,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    ctx.builder
        .build_extract_value(
            base.into_struct_value(),
            index,
            &format!("tuple_elem_{index}"),
        )
        .or_ice()
}
