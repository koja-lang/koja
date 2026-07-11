use inkwell::values::BasicValueEnum;
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantPayload, IRVariantTag};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::LlvmError;

pub(super) const ERR_TAG: IRVariantTag = IRVariantTag(1);
pub(super) const OK_TAG: IRVariantTag = IRVariantTag(0);

pub(super) fn return_symbol(function: &IRFunction) -> Result<&IRSymbol, LlvmError> {
    match &function.return_type {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "`{}` expected a Result return type, got `{other:?}`",
            function.symbol,
        ))),
    }
}

pub(super) fn single_payload_type(
    ctx: &EmitContext<'_>,
    result_symbol: &IRSymbol,
    tag: IRVariantTag,
) -> Result<IRType, LlvmError> {
    let payload = ctx.layouts.enum_variant_payload(result_symbol, tag);
    match payload {
        IRVariantPayload::Tuple(types) if types.len() == 1 => Ok(types[0].clone()),
        other => Err(LlvmError::Codegen(format!(
            "`{result_symbol}` variant {tag:?} should carry one tuple field, got `{other:?}`",
        ))),
    }
}

pub(super) fn build_ok<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    build_enum_value(ctx, result_symbol, OK_TAG, &[value])
}

pub(super) fn build_unit_error<'ctx>(
    ctx: &EmitContext<'ctx>,
    result_symbol: &IRSymbol,
    variant: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let error_type = single_payload_type(ctx, result_symbol, ERR_TAG)?;
    let IRType::Enum(error_symbol) = error_type else {
        return Err(LlvmError::Codegen(format!(
            "`{result_symbol}` Err payload should be an enum, got `{error_type:?}`",
        )));
    };
    let tag = ctx.layouts.enum_variant_tag(&error_symbol, variant);
    let error = build_enum_value(ctx, &error_symbol, tag, &[])?;
    build_enum_value(ctx, result_symbol, ERR_TAG, &[error])
}
