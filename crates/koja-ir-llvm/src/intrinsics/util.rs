//! Helpers every hand-written intrinsic emitter reaches for: typed
//! parameter reads, enum-symbol recovery, aggregate field extraction,
//! the `List` and hashtable value builders, and `ret`. The collection
//! glue emitter ([`crate::emit::collection_glue`]) shares them, so
//! they are crate-visible.
//!
//! A miss in any of these is an upstream IR seal or lower bug, not a
//! user error, so each panics naming the function symbol and the
//! offending slot.

use inkwell::values::{
    BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue, StructValue,
};
use koja_ir::{IRFunction, IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::{hashtable_value_type, list_value_type};

/// Assemble the `{ buf, len, cap }` `List<T>` SSA value.
pub(crate) fn build_list_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    buf: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let list_ty = list_value_type(ctx);
    let with_buf = insert_field(ctx, list_ty.get_undef(), buf, 0, "with_buf")?;
    let with_len = insert_field(ctx, with_buf, len, 1, "with_len")?;
    insert_field(ctx, with_len, cap, 2, "with_cap")
}

/// Assemble the `{ entries, states, len, cap }` hashtable SSA value
/// that `Map` and `Set` share.
pub(crate) fn build_table_struct<'ctx>(
    ctx: &EmitContext<'ctx>,
    entries: PointerValue<'ctx>,
    states: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    cap: IntValue<'ctx>,
) -> Result<StructValue<'ctx>, LlvmError> {
    let table_ty = hashtable_value_type(ctx);
    let with_entries = insert_field(ctx, table_ty.get_undef(), entries, 0, "with_entries")?;
    let with_states = insert_field(ctx, with_entries, states, 1, "with_states")?;
    let with_len = insert_field(ctx, with_states, len, 2, "with_len")?;
    insert_field(ctx, with_len, cap, 3, "with_cap")
}

/// Recover the enum symbol from a slot the lowering pass guarantees
/// is an `IRType::Enum`. `what` names the slot in the panic message.
pub(crate) fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
    what: &str,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => panic!(
            "{what} expected an enum-typed slot, got `{other:?}` (symbol `{}`)",
            function.symbol,
        ),
    }
}

#[track_caller]
pub(crate) fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_extract_value(value, index, name)
        .or_ice()
        .map(|v| v.into_int_value())
}

#[track_caller]
pub(crate) fn extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_extract_value(value, index, name)
        .or_ice()
        .map(|v| v.into_pointer_value())
}

#[track_caller]
fn insert_field<'ctx>(
    ctx: &EmitContext<'ctx>,
    aggregate: StructValue<'ctx>,
    value: impl inkwell::values::BasicValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<StructValue<'ctx>, LlvmError> {
    ctx.builder
        .build_insert_value(aggregate, value, index, name)
        .or_ice()
        .map(|v| v.into_struct_value())
}

pub(crate) fn nth_float<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<FloatValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::FloatValue(v) => Ok(v),
        other => wrong_kind(function, name, "float", other),
    }
}

pub(crate) fn nth_int<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::IntValue(v) => Ok(v),
        other => wrong_kind(function, name, "integer", other),
    }
}

/// The `index`-th LLVM parameter of the intrinsic being emitted.
/// `name` is the source-level parameter name for the panic message.
pub(crate) fn nth_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let param = llvm_function
        .get_nth_param(index)
        .unwrap_or_else(|| panic!("missing param `{name}` (#{index}) on `{}`", function.symbol));
    Ok(param)
}

/// The IR type the seal recorded for the `index`-th parameter.
pub(crate) fn nth_param_type(function: &IRFunction, index: u32) -> Result<&IRType, LlvmError> {
    let ty = function
        .params
        .get(index as usize)
        .map(|param| &param.ty)
        .unwrap_or_else(|| panic!("IR has no param #{index} on `{}`", function.symbol));
    Ok(ty)
}

pub(crate) fn nth_pointer<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::PointerValue(v) => Ok(v),
        other => wrong_kind(function, name, "pointer", other),
    }
}

pub(crate) fn nth_struct<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<StructValue<'ctx>, LlvmError> {
    match nth_param(function, llvm_function, index, name)? {
        BasicValueEnum::StructValue(v) => Ok(v),
        other => wrong_kind(function, name, "struct", other),
    }
}

/// Return `value` from the function being emitted.
#[track_caller]
pub(crate) fn ret<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    ctx.builder.build_return(Some(&value)).or_ice().map(|_| ())
}

fn wrong_kind(function: &IRFunction, name: &str, expected: &str, actual: BasicValueEnum<'_>) -> ! {
    panic!(
        "expected {expected} for `{name}` on `{}`, got `{actual:?}`",
        function.symbol,
    )
}
