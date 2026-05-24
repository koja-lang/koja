//! Pre-emit phase for [`koja_ir::IRStructDecl`]: register one
//! LLVM `StructType` per decl on [`super::TypeLayouts`].
//!
//! Two-phase across all packages so a struct can carry another
//! struct as a field regardless of declaration order:
//! [`declare_struct_type`] mints opaque placeholders;
//! [`define_struct_body`] sets each body once every package's
//! placeholders exist.

use inkwell::types::BasicTypeEnum;
use koja_ir::IRStructDecl;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::value_basic_type;

pub(crate) fn declare_struct_type<'ctx>(ctx: &EmitContext<'ctx>, decl: &IRStructDecl) {
    let llvm_struct = ctx.context.opaque_struct_type(decl.symbol.mangled());
    ctx.layouts
        .register_struct_type(decl.symbol.clone(), llvm_struct);
    let field_types = decl.fields.iter().map(|f| f.ir_type.clone()).collect();
    ctx.layouts
        .register_struct_fields(decl.symbol.clone(), field_types);
}

pub(crate) fn define_struct_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    decl: &IRStructDecl,
) -> Result<(), LlvmError> {
    let mut body: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(decl.fields.len());
    for field in &decl.fields {
        body.push(value_basic_type(ctx, &field.ir_type)?);
    }
    let llvm_struct = ctx.layouts.struct_type(decl.symbol.mangled());
    llvm_struct.set_body(&body, false);
    Ok(())
}
