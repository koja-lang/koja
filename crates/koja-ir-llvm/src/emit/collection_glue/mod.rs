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
//!   references — heap leaves bump their rc, composite elements recurse
//!   through their own `clone_T`. Scalar elements need nothing beyond
//!   the `memcpy`.
//! - **drop** *releases* each element (rc-decrement leaves, recurse
//!   into `drop_T` for composites) then `free`s the backing buffer.
//!
//! This module owns the dispatch entry point, the shared element
//! *disposition* ([`acquire_element`] / [`release_element`]), and the
//! low-level buffer helpers both shapes share. The per-collection
//! bodies live in [`list`] (the dynamic-array walk) and [`table`] (the
//! open-addressed `Map` / `Set` bucket walk).
//!
//! Element disposition is read off the declared-functions index: a
//! heap leaf bumps/decrements its rc inline; a composite carrying glue
//! (`clone_glue_symbol` is declared) recurses; anything else is a
//! trivially-copyable scalar and the `memcpy` already did the work.

mod list;
mod table;

use inkwell::AddressSpace;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue, StructValue};
use koja_ir::mangling::{clone_glue_symbol, drop_glue_symbol};
use koja_ir::{FunctionKind, IRFunction, IRType};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_base, is_heap_leaf};
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{declare_rc_dec_extern, declare_rc_inc_extern};
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
        (_, IRType::Indirect(_)) => todo!("Indirect box glue body emission"),
        (kind, other) => panic!(
            "collection glue `{}`: unexpected ({kind:?}, operand {other:?}) — \
             only collection / box operands lower with empty blocks",
            function.symbol,
        ),
    }
}

/// Acquire the element at `slot` (a pointer into the freshly-cloned
/// buffer): bump a heap leaf's rc, or overwrite the slot with a deep
/// clone for a composite. Scalars need nothing — the `memcpy` already
/// copied them.
pub(super) fn acquire_element<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, function, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[base.into()], "elem.rc_inc")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element rc_inc for `{}`", function.symbol), e))
    } else if let Some(clone_glue) = ctx.declared_function(&clone_glue_symbol(element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let original = ctx
            .builder
            .build_load(element_ty, slot, "elem")
            .map_err(|e| inkwell_err(format_args!("element load for `{}`", function.symbol), e))?;
        let cloned = ctx
            .builder
            .build_call(clone_glue, &[original.into()], "elem.clone")
            .map_err(|e| inkwell_err(format_args!("element clone for `{}`", function.symbol), e))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                LlvmError::Codegen(format!(
                    "clone glue returned void for `{}`",
                    function.symbol
                ))
            })?;
        ctx.builder
            .build_store(slot, cloned)
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element store for `{}`", function.symbol), e))
    } else {
        Ok(())
    }
}

/// Release the element at `slot`: rc-decrement a heap leaf, recurse
/// into `drop_T` for a composite, or do nothing for a scalar.
pub(super) fn release_element<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, function, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_dec = declare_rc_dec_extern(ctx);
        ctx.builder
            .build_call(rc_dec, &[base.into()], "elem.rc_dec")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element rc_dec for `{}`", function.symbol), e))
    } else if let Some(drop_glue) = ctx.declared_function(&drop_glue_symbol(element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let value = ctx
            .builder
            .build_load(element_ty, slot, "elem")
            .map_err(|e| inkwell_err(format_args!("element load for `{}`", function.symbol), e))?;
        ctx.builder
            .build_call(drop_glue, &[value.into()], "elem.drop")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element drop for `{}`", function.symbol), e))
    } else {
        Ok(())
    }
}

/// Pointer to element `index` in a byte-addressed buffer.
pub(super) fn element_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    buf: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    element_size: IntValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let offset = ctx
        .builder
        .build_int_mul(index, element_size, "elem.off")
        .map_err(|e| inkwell_err(format_args!("element offset for `{}`", function.symbol), e))?;
    unsafe {
        ctx.builder
            .build_gep(i8_ty, buf, &[offset], "elem.ptr")
            .map_err(|e| inkwell_err(format_args!("element GEP for `{}`", function.symbol), e))
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

pub(super) fn load_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    slot: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_load(ptr_ty, slot, name)
        .map(|v| v.into_pointer_value())
        .map_err(|e| {
            inkwell_err(
                format_args!("element ptr load for `{}`", function.symbol),
                e,
            )
        })
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
