//! Emit-time synthesis of *collection* clone / deep-copy / drop glue
//! bodies (`List` / `Map` / `Set`). The `elaborate` sub-pass registers
//! these as [`FunctionKind::CloneGlue`] / [`FunctionKind::DeepCopyGlue`]
//! / [`FunctionKind::DropGlue`] shells with empty `blocks`. Unlike
//! aggregate glue (whose CFG `elaborate` synthesizes in IR), a
//! collection's body is a runtime-shaped buffer walk we build straight
//! from the operand type here.
//!
//! Memory model is deep ownership (no buffer refcount):
//!
//! - **clone** mallocs a fresh buffer, `memcpy`s the element bytes,
//!   then *acquires* each element so the copy owns independent
//!   references.
//! - **deep copy** is the same walk with the per-element acquire
//!   swapped for a deep copy, so the result shares no storage with
//!   the source (process-boundary hand-off).
//! - **drop** *releases* each element then `free`s the backing buffer.
//!
//! The per-element ops live in [`crate::intrinsics::element`]
//! (shared with the copy-on-write mutators). This module owns the
//! dispatch entry point plus the collection-struct field helpers. The
//! per-collection bodies live in [`list`] (the dynamic-array walk) and
//! [`table`] (the open-addressed `Map` / `Set` bucket walk).

mod list;
mod table;

use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::{FunctionKind, IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::ir_basic_type;

/// Which per-element ownership op a collection copy body runs after
/// `memcpy`ing the buffer(s): `Acquire` for clone glue (rc sharing)
/// or `Deep` for deep-copy glue (physically independent storage).
#[derive(Clone, Copy)]
pub(super) enum ElementCopy {
    Acquire,
    Deep,
}

/// Entry point from [`crate::function::define_function`] for an
/// empty-block [`FunctionKind::CloneGlue`] / [`FunctionKind::DeepCopyGlue`]
/// / [`FunctionKind::DropGlue`] shell. Appends the single `entry`
/// block and dispatches on the operand type carried by `params[0]`.
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
            list::copy_list(ctx, function, llvm_function, element, ElementCopy::Acquire)
        }
        (FunctionKind::DeepCopyGlue, IRType::List(element)) => {
            list::copy_list(ctx, function, llvm_function, element, ElementCopy::Deep)
        }
        (FunctionKind::DropGlue, IRType::List(element)) => {
            list::drop_list(ctx, function, llvm_function, element)
        }
        (FunctionKind::CloneGlue, IRType::Set(element)) => table::copy_table(
            ctx,
            function,
            llvm_function,
            element,
            None,
            ElementCopy::Acquire,
        ),
        (FunctionKind::DeepCopyGlue, IRType::Set(element)) => table::copy_table(
            ctx,
            function,
            llvm_function,
            element,
            None,
            ElementCopy::Deep,
        ),
        (FunctionKind::DropGlue, IRType::Set(element)) => {
            table::drop_table(ctx, function, llvm_function, element, None)
        }
        (FunctionKind::CloneGlue, IRType::Map { key, value }) => table::copy_table(
            ctx,
            function,
            llvm_function,
            key,
            Some(value),
            ElementCopy::Acquire,
        ),
        (FunctionKind::DeepCopyGlue, IRType::Map { key, value }) => table::copy_table(
            ctx,
            function,
            llvm_function,
            key,
            Some(value),
            ElementCopy::Deep,
        ),
        (FunctionKind::DropGlue, IRType::Map { key, value }) => {
            table::drop_table(ctx, function, llvm_function, key, Some(value))
        }
        (kind, other) => panic!(
            "collection glue `{}`: unexpected ({kind:?}, operand {other:?}). \
             Only collection operands lower with empty blocks (`Indirect` is \
             transparent and carries no glue of its own)",
            function.symbol,
        ),
    }
}

/// ABI byte size of `ty` on the host triple: the same target-data
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

#[track_caller]
pub(super) fn extract_int<'ctx>(
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
pub(super) fn extract_pointer<'ctx>(
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
pub(super) fn call_ptr<'ctx>(
    ctx: &EmitContext<'ctx>,
    callee: FunctionValue<'ctx>,
    args: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    Ok(ctx.call_basic(callee, args, name)?.into_pointer_value())
}
