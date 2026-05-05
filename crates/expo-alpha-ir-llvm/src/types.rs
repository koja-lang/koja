//! Translate alpha [`expo_alpha_ir::IRType`] values into the
//! corresponding inkwell types. `Bool` maps to `i1`; signed and
//! unsigned widths share their LLVM width (signedness is
//! per-instruction, not per-type); `String` maps to a default-AS
//! pointer (the v1 header layout lives in
//! [`crate::emit::instruction::emit_const_string`]).

use expo_alpha_ir::IRType;
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{BasicTypeEnum, IntType};

use crate::error::LlvmError;

/// LLVM integer type for an integer-family or `Bool` [`IRType`].
/// `Unit` and non-integer variants (`String`) surface as a feature-gap
/// diagnostic — call sites that genuinely need an int (binary / unary
/// ops) hit this; sites that accept any basic type use [`ir_basic_type`].
pub(crate) fn ir_int_type<'ctx>(
    context: &'ctx Context,
    ty: &IRType,
) -> Result<IntType<'ctx>, LlvmError> {
    match ty {
        IRType::Bool => Ok(context.bool_type()),
        IRType::Int8 | IRType::UInt8 => Ok(context.i8_type()),
        IRType::Int16 | IRType::UInt16 => Ok(context.i16_type()),
        IRType::Int32 | IRType::UInt32 => Ok(context.i32_type()),
        IRType::Int64 | IRType::UInt64 => Ok(context.i64_type()),
        IRType::String => Err(LlvmError::Codegen(
            "expected an integer or Bool IRType, got `String`".to_string(),
        )),
        IRType::Unit => Err(LlvmError::Codegen(
            "expected an integer or Bool IRType, got `Unit`".to_string(),
        )),
    }
}

/// LLVM basic type for any [`IRType`] that has a value-level
/// representation. `Unit` is rejected (no LLVM type); ints / `Bool`
/// route through [`ir_int_type`]; `String` is a default-AS pointer.
pub(crate) fn ir_basic_type<'ctx>(
    context: &'ctx Context,
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
        | IRType::UInt64 => Ok(ir_int_type(context, ty)?.into()),
        IRType::String => Ok(context.ptr_type(AddressSpace::default()).into()),
        IRType::Unit => Err(LlvmError::Codegen(
            "expected a value-level IRType, got `Unit`".to_string(),
        )),
    }
}
