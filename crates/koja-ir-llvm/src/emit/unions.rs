//! Union literal + projection emission: `UnionWrap`, `UnionTagGet`,
//! `UnionPayloadGet`. Every shape spills the SSA value through an
//! entry-block alloca and GEPs through the union's outer
//! `{ i8 tag, [N x i8] payload }` struct.

use inkwell::values::BasicValueEnum;
use koja_ir::IRType;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::ir_basic_type;

/// Materialize a union value: alloca the outer struct, write the
/// tag at field 0, write the typed payload at field 1, and load
/// the populated outer back as the SSA result. Backends key the
/// outer's name through the union's mangled symbol carried on
/// `IRType::Union`.
pub(super) fn emit_union_wrap<'ctx>(
    ctx: &EmitContext<'ctx>,
    member_index: u8,
    _member_type: &IRType,
    ty: &IRType,
    payload: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let IRType::Union { mangled, .. } = ty else {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: UnionWrap target IRType is not Union (got `{ty:?}`)",
        )));
    };
    let (outer, _payload_size) = ctx.layouts.union_outer(mangled.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{mangled}_tmp"));
    let tag_ptr = ctx
        .builder
        .build_struct_gep(outer, alloca, 0, &format!("{mangled}_tag_ptr"))
        .or_ice()?;
    let tag_value = ctx
        .context
        .i8_type()
        .const_int(u64::from(member_index), false);
    ctx.builder.build_store(tag_ptr, tag_value).or_ice()?;
    let payload_ptr = ctx
        .builder
        .build_struct_gep(outer, alloca, 1, &format!("{mangled}_payload_ptr"))
        .or_ice()?;
    ctx.builder.build_store(payload_ptr, payload).or_ice()?;
    ctx.builder
        .build_load(outer, alloca, mangled.mangled())
        .or_ice()
}

/// Spill `value` to a fresh outer-typed alloca and load the tag
/// byte at field 0 as `i8`. Counterpart of
/// [`super::enums::emit_enum_tag_get`] for the union family.
pub(super) fn emit_union_tag_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let IRType::Union { mangled, .. } = ty else {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: UnionTagGet receiver IRType is not Union (got `{ty:?}`)",
        )));
    };
    let (outer, _) = ctx.layouts.union_outer(mangled.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{mangled}_tag_src"));
    ctx.builder.build_store(alloca, value).or_ice()?;
    let tag_ptr = ctx
        .builder
        .build_struct_gep(outer, alloca, 0, &format!("{mangled}_tag_ptr"))
        .or_ice()?;
    ctx.builder
        .build_load(ctx.context.i8_type(), tag_ptr, &format!("{mangled}_tag"))
        .or_ice()
}

/// Spill `value` to a fresh outer-typed alloca, GEP into the
/// payload buffer at field 1, and load it as `member_type`. The
/// caller (the `match` driver) gates this on a successful tag-eq
/// test, so the payload bytes are guaranteed to encode a value of
/// `member_type`.
pub(super) fn emit_union_payload_get<'ctx>(
    ctx: &EmitContext<'ctx>,
    member_type: &IRType,
    ty: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let IRType::Union { mangled, .. } = ty else {
        return Err(LlvmError::Codegen(format!(
            "LLVM emit: UnionPayloadGet receiver IRType is not Union (got `{ty:?}`)",
        )));
    };
    let (outer, _) = ctx.layouts.union_outer(mangled.mangled());
    let alloca = ctx.build_entry_alloca(outer, &format!("{mangled}_payload_src"));
    ctx.builder.build_store(alloca, value).or_ice()?;
    let payload_ptr = ctx
        .builder
        .build_struct_gep(outer, alloca, 1, &format!("{mangled}_payload_ptr"))
        .or_ice()?;
    let member_llvm_type = ir_basic_type(ctx, member_type)?;
    ctx.builder
        .build_load(member_llvm_type, payload_ptr, &format!("{mangled}_payload"))
        .or_ice()
}
