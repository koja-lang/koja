//! Translate alpha [`expo_alpha_ir::IRType`] values into the
//! corresponding inkwell types. `Bool` maps to `i1`; signed and
//! unsigned widths share their LLVM width (signedness is
//! per-instruction, not per-type); `Float32` / `Float64` map to
//! `f32` / `f64` IEEE 754; `String` maps to a default-AS pointer
//! (the v1 header layout lives in
//! [`crate::emit::instruction::emit_const_string`]); `Struct(_)`
//! resolves through the pre-emitted [`crate::ctx::EmitContext`] struct
//! type map; `Enum(_)` resolves through the pre-emitted enum-layout
//! map ([`crate::ctx::EmitContext::enum_outer_type`]).
//!
//! [`ir_byte_size`] / [`ir_alignment`] are the target-aware adapters
//! the enum-layout pre-emit phase calls to compute per-variant
//! padding and the outer blob's max-alignment chunk. They route
//! through the host [`inkwell::targets::TargetData`] pinned on the
//! [`crate::ctx::EmitContext`] so the layout matches the object emitter's
//! ABI rather than a hard-coded 64-bit assumption.

use expo_alpha_ir::IRType;
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType, StructType,
};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

/// LLVM integer type for an integer-family or `Bool` [`IRType`].
/// Float / `String` / `Unit` variants surface as a feature-gap
/// diagnostic — call sites that genuinely need an int (e.g. cond
/// branches, where the seal pass guarantees an `i1`) hit this;
/// sites that accept any basic type use [`ir_basic_type`].
pub(crate) fn ir_int_type<'ctx>(
    context: &'ctx Context,
    ty: &IRType,
) -> Result<IntType<'ctx>, LlvmError> {
    match ty {
        IRType::Binary | IRType::Bits => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`"
        ))),
        IRType::Bool => Ok(context.bool_type()),
        IRType::CPtr(_) => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`"
        ))),
        IRType::Enum(_) => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`"
        ))),
        IRType::Float32 | IRType::Float64 => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`"
        ))),
        IRType::Function { .. } => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`",
        ))),
        IRType::Int8 | IRType::UInt8 => Ok(context.i8_type()),
        IRType::Int16 | IRType::UInt16 => Ok(context.i16_type()),
        IRType::Int32 | IRType::UInt32 => Ok(context.i32_type()),
        IRType::Int64 | IRType::UInt64 => Ok(context.i64_type()),
        IRType::String => Err(LlvmError::Codegen(
            "expected an integer or Bool IRType, got `String`".to_string(),
        )),
        IRType::Struct(_) => Err(LlvmError::Codegen(format!(
            "expected an integer or Bool IRType, got `{ty:?}`",
        ))),
        IRType::Unit => Err(LlvmError::Codegen(
            "expected an integer or Bool IRType, got `Unit`".to_string(),
        )),
    }
}

/// LLVM basic type for any [`IRType`] that has a value-level
/// representation. `Unit` is rejected (no LLVM type); ints / `Bool`
/// route through [`ir_int_type`]; `Float32` / `Float64` map to
/// `f32` / `f64`; `String` is a default-AS pointer; `Struct(symbol)`
/// resolves through [`EmitContext::struct_type`] (registered by the
/// pre-emit phase); `Enum(symbol)` resolves through
/// [`EmitContext::enum_outer_type`] (the outer opaque blob registered
/// by [`crate::layout::enums::declare_enum_type`] +
/// [`crate::layout::enums::define_enum_bodies`]).
pub(crate) fn ir_basic_type<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRType,
) -> Result<BasicTypeEnum<'ctx>, LlvmError> {
    match ty {
        IRType::Bool
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64 => Ok(ir_int_type(ctx.context, ty)?.into()),
        IRType::Binary | IRType::Bits | IRType::String => {
            Ok(ctx.context.ptr_type(AddressSpace::default()).into())
        }
        IRType::CPtr(_) => Ok(ctx.context.ptr_type(AddressSpace::default()).into()),
        IRType::Enum(symbol) => Ok(ctx.layouts.enum_outer_type(symbol.mangled()).into()),
        IRType::Float32 => Ok(ctx.context.f32_type().into()),
        IRType::Float64 => Ok(ctx.context.f64_type().into()),
        IRType::Function { .. } => Ok(closure_fat_ptr_type(ctx).into()),
        IRType::Struct(symbol) => Ok(ctx.layouts.struct_type(symbol.mangled()).into()),
        IRType::Unit => Err(LlvmError::Codegen(
            "expected a value-level IRType, got `Unit`".to_string(),
        )),
    }
}

/// ABI byte size of `ty` on the host triple. Routes through
/// [`inkwell::targets::TargetData::get_abi_size`] so the result
/// matches what the object emitter will lay out (e.g. an
/// `IRType::String` pointer is 8 bytes on 64-bit hosts and 4 on
/// 32-bit hosts; an `IRType::Enum(_)` is the size of its outer
/// blob, computed by the pre-emit phase).
///
/// Public for follow-up enum-shaped emit work (eq, destructure,
/// pattern match) that needs the same target-aware sizing the
/// per-variant layout already uses inline.
#[allow(dead_code)]
pub(crate) fn ir_byte_size<'ctx>(ctx: &EmitContext<'ctx>, ty: &IRType) -> Result<u64, LlvmError> {
    let basic = ir_basic_type(ctx, ty)?;
    Ok(ctx.layouts.target_data.get_abi_size(&basic))
}

/// ABI alignment of `ty` on the host triple. Sibling of
/// [`ir_byte_size`]. The enum layout queries `target_data`
/// directly during the pre-emit phase (it already has
/// `BasicTypeEnum` handles in scope); this helper is kept for the
/// same follow-up emit work [`ir_byte_size`] is reserved for.
#[allow(dead_code)]
pub(crate) fn ir_alignment<'ctx>(ctx: &EmitContext<'ctx>, ty: &IRType) -> Result<u32, LlvmError> {
    let basic = ir_basic_type(ctx, ty)?;
    Ok(ctx.layouts.target_data.get_abi_alignment(&basic))
}

/// Closure value shape: `{ fn_ptr, env_ptr }`. Anonymous (literal)
/// struct so two `IRType::Function` operands of compatible shape
/// share a single LLVM type without a named-type registry. Both
/// fields are opaque pointers in the default address space; the
/// fn_ptr's signature is reconstructed at each indirect call site
/// via [`closure_body_signature`].
pub(crate) fn closure_fat_ptr_type<'ctx>(ctx: &EmitContext<'ctx>) -> StructType<'ctx> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.context
        .struct_type(&[ptr_ty.into(), ptr_ty.into()], false)
}

/// LLVM `FunctionType` for a closure body's signature. Prepends an
/// `env_ptr: i8*` parameter to the user-visible params; mirrors the
/// closure-kind ABI declared by [`crate::function::declare_function`].
/// Used by `MakeClosure` (to wire the indirect-call signature) and
/// by `CallClosure` (to call through a fat-pointer's fn_ptr).
pub(crate) fn closure_body_signature<'ctx>(
    ctx: &EmitContext<'ctx>,
    user_params: &[IRType],
    return_type: &IRType,
) -> Result<FunctionType<'ctx>, LlvmError> {
    let env_param: BasicMetadataTypeEnum<'ctx> =
        ctx.context.ptr_type(AddressSpace::default()).into();
    let mut metadata: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(user_params.len() + 1);
    metadata.push(env_param);
    for param in user_params {
        metadata.push(ir_basic_type(ctx, param)?.into());
    }
    Ok(if matches!(return_type, IRType::Unit) {
        ctx.context.void_type().fn_type(&metadata, false)
    } else {
        ir_basic_type(ctx, return_type)?.fn_type(&metadata, false)
    })
}

/// LLVM struct type for the env block of a closure with the given
/// `env_layout`. Anonymous (literal) struct so each capture-arity /
/// type combination shares one LLVM type per emit module without a
/// named-type registry. Empty layouts produce an empty `{}`
/// struct — the env pointer is null in that case (see
/// `MakeClosure`'s captureless path).
pub(crate) fn env_struct_type<'ctx>(
    ctx: &EmitContext<'ctx>,
    env_layout: &[IRType],
) -> Result<StructType<'ctx>, LlvmError> {
    let mut fields: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(env_layout.len());
    for ty in env_layout {
        fields.push(ir_basic_type(ctx, ty)?);
    }
    Ok(ctx.context.struct_type(&fields, false))
}
