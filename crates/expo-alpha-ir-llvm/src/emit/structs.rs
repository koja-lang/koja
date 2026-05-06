//! Pre-emit phase for [`expo_alpha_ir::IRStructDecl`]: build one
//! LLVM `StructType` per struct declaration and register it on the
//! [`EmitCtx`] so [`crate::types::ir_basic_type`] can resolve
//! `IRType::Struct(symbol)` when sizing function signatures, allocas,
//! and field accesses.
//!
//! Two-phase across all packages so a struct can carry another
//! struct as a field regardless of declaration order:
//!
//! 1. [`declare_struct_types`] mints opaque `%Pkg.Name = type opaque`
//!    placeholders for every struct in every package and registers
//!    each on `EmitCtx`.
//! 2. [`define_struct_bodies`] revisits each decl and calls
//!    `set_body` with each field's translated [`IRType`], using
//!    [`crate::types::ir_basic_type`] so cross-struct references
//!    resolve through the placeholders the first phase registered.

use expo_alpha_ir::IRStructDecl;
use inkwell::types::BasicTypeEnum;

use crate::ctx::EmitCtx;
use crate::error::LlvmError;
use crate::types::ir_basic_type;

/// Mint an opaque `StructType` for `decl` and register it on `ctx`.
/// The body is set in the second phase so cross-struct field types
/// can resolve regardless of declaration order.
pub(crate) fn declare_struct_type<'ctx>(ctx: &EmitCtx<'ctx>, decl: &IRStructDecl) {
    let llvm_struct = ctx.context.opaque_struct_type(decl.symbol.mangled());
    ctx.register_struct_type(decl.symbol.clone(), llvm_struct);
}

/// Set the body of the `StructType` registered for `decl` to the
/// translated field types. Must run after every package's structs
/// have been declared via [`declare_struct_type`].
pub(crate) fn define_struct_body<'ctx>(
    ctx: &EmitCtx<'ctx>,
    decl: &IRStructDecl,
) -> Result<(), LlvmError> {
    let mut body: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(decl.fields.len());
    for field in &decl.fields {
        body.push(ir_basic_type(ctx, &field.ir_type)?);
    }
    let llvm_struct = ctx.struct_type(decl.symbol.mangled());
    llvm_struct.set_body(&body, false);
    Ok(())
}
