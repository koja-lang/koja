//! Raw cursor scan shared by `Map.next` and `Set.next`.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, StructValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantPayload};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::acquire_value;
use crate::types::{ir_basic_type, tuple_struct_type};

use super::util::{entry_pointer, expect_enum_symbol, extract_table_fields, nth_param, value_slot};
use super::{HashtableLayout, STATE_OCCUPIED};
use crate::intrinsics::option;

pub(crate) fn emit_next<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let option_symbol = expect_enum_symbol(&function.return_type, function, "collection.next")?;
    let payload_type = option_payload_type(ctx, option_symbol, function)?;
    let table = extract_table_fields(ctx, function, llvm_function)?;
    let cursor = nth_param(function, llvm_function, 1, "cursor")?.into_int_value();
    let entry_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "collection.next has no entry block on `{}`",
            function.symbol,
        ))
    })?;

    let scan = ctx.context.append_basic_block(llvm_function, "cursor.scan");
    let check = ctx
        .context
        .append_basic_block(llvm_function, "cursor.check");
    let found = ctx
        .context
        .append_basic_block(llvm_function, "cursor.found");
    let advance = ctx
        .context
        .append_basic_block(llvm_function, "cursor.advance");
    let none = ctx.context.append_basic_block(llvm_function, "cursor.none");

    let nonnegative = ctx
        .builder
        .build_int_compare(
            IntPredicate::SGE,
            cursor,
            i64_ty.const_zero(),
            "cursor.nonnegative",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(nonnegative, scan, none)
        .or_ice()?;

    ctx.builder.position_at_end(scan);
    let slot_phi = ctx.builder.build_phi(i64_ty, "cursor.slot").or_ice()?;
    slot_phi.add_incoming(&[(&cursor, entry_block)]);
    let slot = slot_phi.as_basic_value().into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, slot, table.capacity, "cursor.in_range")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(in_range, check, none)
        .or_ice()?;

    ctx.builder.position_at_end(check);
    let state_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, table.states_ptr, &[slot], "cursor.state_ptr")
            .or_ice()?
    };
    let state = ctx
        .builder
        .build_load(i8_ty, state_ptr, "cursor.state")
        .or_ice()?
        .into_int_value();
    let occupied = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            state,
            i8_ty.const_int(STATE_OCCUPIED, false),
            "cursor.occupied",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(occupied, found, advance)
        .or_ice()?;

    ctx.builder.position_at_end(advance);
    let next_slot = ctx
        .builder
        .build_int_add(slot, i64_ty.const_int(1, false), "cursor.next_slot")
        .or_ice()?;
    slot_phi.add_incoming(&[(&next_slot, advance)]);
    ctx.builder.build_unconditional_branch(scan).or_ice()?;

    ctx.builder.position_at_end(found);
    let entry_ptr = entry_pointer(ctx, table.entries_ptr, slot, layout.entry_size)?;
    let key = ctx
        .builder
        .build_load(ir_basic_type(ctx, layout.key_ty)?, entry_ptr, "cursor.key")
        .or_ice()?;
    let key = acquire_value(ctx, layout.key_ty, key)?;
    let item: BasicValueEnum<'ctx> = if let Some(value_ty) = layout.value_ty {
        let value_ptr = value_slot(ctx, entry_ptr, layout.key_size)?;
        let value = ctx
            .builder
            .build_load(ir_basic_type(ctx, value_ty)?, value_ptr, "cursor.value")
            .or_ice()?;
        let value = acquire_value(ctx, value_ty, value)?;
        build_tuple(
            ctx,
            &[layout.key_ty.clone(), value_ty.clone()],
            &[key, value],
        )?
        .into()
    } else {
        key
    };
    let next_cursor = ctx
        .builder
        .build_int_add(slot, i64_ty.const_int(1, false), "cursor.result_next")
        .or_ice()?;
    let payload = build_tuple(
        ctx,
        tuple_elements(&payload_type, function)?,
        &[item, next_cursor.into()],
    )?;
    let some = build_enum_value(
        ctx,
        option_symbol,
        option::some_tag(ctx, option_symbol),
        &[payload.into()],
    )?;
    ctx.builder.build_return(Some(&some)).or_ice()?;

    ctx.builder.position_at_end(none);
    let none_value = build_enum_value(
        ctx,
        option_symbol,
        option::none_tag(ctx, option_symbol),
        &[],
    )?;
    ctx.builder
        .build_return(Some(&none_value))
        .or_ice()
        .map(|_| ())
}

fn option_payload_type(
    ctx: &EmitContext<'_>,
    option_symbol: &IRSymbol,
    function: &IRFunction,
) -> Result<IRType, LlvmError> {
    match ctx
        .layouts
        .enum_variant_payload(option_symbol, option::some_tag(ctx, option_symbol))
    {
        IRVariantPayload::Tuple(types) if types.len() == 1 => Ok(types.into_iter().next().unwrap()),
        other => Err(LlvmError::Codegen(format!(
            "collection.next on `{}` has unexpected Option.Some payload `{other:?}`",
            function.symbol,
        ))),
    }
}

fn tuple_elements<'a>(ty: &'a IRType, function: &IRFunction) -> Result<&'a [IRType], LlvmError> {
    match ty {
        IRType::Tuple(elements) if elements.len() == 2 => Ok(elements),
        other => Err(LlvmError::Codegen(format!(
            "collection.next on `{}` expected a two-element tuple payload, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn build_tuple<'ctx>(
    ctx: &EmitContext<'ctx>,
    element_types: &[IRType],
    values: &[BasicValueEnum<'ctx>],
) -> Result<StructValue<'ctx>, LlvmError> {
    let tuple_type = tuple_struct_type(ctx, element_types)?;
    let mut tuple = tuple_type.get_undef();
    for (index, value) in values.iter().enumerate() {
        tuple = ctx
            .builder
            .build_insert_value(tuple, *value, index as u32, "cursor.tuple")
            .or_ice()?
            .into_struct_value();
    }
    Ok(tuple)
}
