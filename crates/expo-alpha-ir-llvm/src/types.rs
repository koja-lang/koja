//! Translate alpha [`expo_alpha_ir::IRType`] values into the
//! corresponding inkwell types.
//!
//! Integer widths map directly onto LLVM `iN` types (signedness lives
//! on instructions, not types — same as LLVM and Cranelift). The
//! slice's seal pass only admits `Bool` / `Int64` / `Unit`, but
//! `ir_int_type` already covers all eight integer variants so the
//! follow-up width slice is a registry update, not an LLVM change.

use expo_alpha_ir::IRType;
use inkwell::context::Context;
use inkwell::types::IntType;

use crate::error::LlvmError;

/// LLVM integer type for an integer-family [`IRType`].
pub(crate) fn ir_int_type<'ctx>(
    context: &'ctx Context,
    ty: &IRType,
) -> Result<IntType<'ctx>, LlvmError> {
    match ty {
        IRType::Int8 | IRType::UInt8 => Ok(context.i8_type()),
        IRType::Int16 | IRType::UInt16 => Ok(context.i16_type()),
        IRType::Int32 | IRType::UInt32 => Ok(context.i32_type()),
        IRType::Int64 | IRType::UInt64 => Ok(context.i64_type()),
        other => Err(LlvmError::Codegen(format!(
            "expected an integer IRType, got `{other:?}`",
        ))),
    }
}
