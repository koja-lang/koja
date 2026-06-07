//! Emit-time synthesis of *collection* clone / drop glue bodies
//! (`List` / `Map` / `Set`). The `elaborate` sub-pass registers these
//! as [`FunctionKind::CloneGlue`] / [`FunctionKind::DropGlue`] shells
//! with empty `blocks`; unlike aggregate glue (whose CFG `elaborate`
//! synthesizes in IR), a collection's body is a runtime-shaped buffer
//! walk we build straight from the operand type here.
//!
//! Memory model is deep ownership (no buffer refcount):
//!
//! - **clone** mallocs a fresh buffer, `memcpy`s the element bytes,
//!   then *acquires* each element so the copy owns independent
//!   references.
//! - **drop** *releases* each element then `free`s the backing buffer.
//!
//! The per-element acquire / release lives in [`crate::intrinsics::element`]
//! (shared with the copy-on-write mutators). This module owns the
//! dispatch entry point plus the collection-struct field helpers; the
//! per-collection bodies live in [`list`] (the dynamic-array walk) and
//! [`table`] (the open-addressed `Map` / `Set` bucket walk).

mod list;
mod table;

use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{FunctionKind, IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

/// Entry point from [`crate::function::define_function`] for an
/// empty-block [`FunctionKind::CloneGlue`] / [`FunctionKind::DropGlue`]
/// shell. Appends the single `entry` block and dispatches on the
/// operand type carried by `params[0]`.
pub(crate) fn emit_collection_glue_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let operand = &function.params[0].ty;
    match (&function.kind, operand) {
        (FunctionKind::CloneGlue, IRType::List(element)) => {
            list::clone_list(ctx, function, llvm_function, element)
        }
        (FunctionKind::DropGlue, IRType::List(element)) => {
            list::drop_list(ctx, function, llvm_function, element)
        }
        (FunctionKind::CloneGlue, IRType::Set(element)) => {
            table::clone_table(ctx, function, llvm_function, element, None)
        }
        (FunctionKind::DropGlue, IRType::Set(element)) => {
            table::drop_table(ctx, function, llvm_function, element, None)
        }
        (FunctionKind::CloneGlue, IRType::Map { key, value }) => {
            table::clone_table(ctx, function, llvm_function, key, Some(value))
        }
        (FunctionKind::DropGlue, IRType::Map { key, value }) => {
            table::drop_table(ctx, function, llvm_function, key, Some(value))
        }
        (kind, other) => panic!(
            "collection glue `{}`: unexpected ({kind:?}, operand {other:?}) — \
             only collection operands lower with empty blocks (`Indirect` is \
             transparent and carries no glue of its own)",
            function.symbol,
        ),
    }
}

/// ABI byte size of `ty` on the host triple — the same target-data
/// the rest of the layout pipeline (and the hashtable intrinsics) read,
/// so glue buffer arithmetic matches the emitted field sizes exactly.
pub(super) fn abi_size<'ctx>(ctx: &EmitContext<'ctx>, ty: &IRType) -> Result<u64, LlvmError> {
    Ok(ctx
        .layouts
        .target_data
        .get_abi_size(&ir_basic_type(ctx, ty)?))
}

pub(super) fn nth_struct<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
) -> Result<StructValue<'ctx>, LlvmError> {
    llvm_function
        .get_nth_param(index)
        .map(|p| p.into_struct_value())
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "collection glue `{}` missing operand param #{index}",
                function.symbol,
            ))
        })
}

pub(super) fn extract_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_extract_value(value, index, name)
        .map(|v| v.into_int_value())
        .map_err(|e| inkwell_err(format_args!("extract int for `{}`", function.symbol), e))
}

pub(super) fn extract_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    value: StructValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_extract_value(value, index, name)
        .map(|v| v.into_pointer_value())
        .map_err(|e| inkwell_err(format_args!("extract ptr for `{}`", function.symbol), e))
}

pub(super) fn call_ptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    callee: FunctionValue<'ctx>,
    args: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_call(callee, args, name)
        .map_err(|e| inkwell_err(format_args!("call for `{}`", function.symbol), e))?
        .try_as_basic_value()
        .basic()
        .map(|v| v.into_pointer_value())
        .ok_or_else(|| LlvmError::Codegen(format!("call returned void for `{}`", function.symbol)))
}
