//! `Int.parse(input: String) -> Result<Int, NumericConversionError>`
//! and `Float.parse(input: String) -> Result<Float, NumericConversionError>`.
//!
//! Both delegate to runtime helpers (`koja_int_parse` /
//! `koja_float_parse`) that take an Koja string payload pointer and
//! an out-pointer for the parsed scalar, returning a
//! `koja-runtime` `parse_text` code: ok, invalid format, or out of
//! range (a well-formed number that doesn't fit the target — an
//! overflowing integer, or a float magnitude that rounds to
//! infinity). The intrinsic body allocates an entry-block out slot,
//! calls the helper, switches on the code, and wraps the parsed
//! value into `Result.Ok(_)` or the matching
//! `NumericConversionError` variant into `Result.Err(_)` via
//! [`super::numeric::build_conversion_error`].

use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, ParseTarget};

use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::numeric::build_conversion_error;
use crate::runtime::{declare_float_parse_extern, declare_int_parse_extern};

/// `enum Result<T, E>` variant tag for `Ok(T)` — declaration order
/// in `koja/lib/global/src/kernel.koja`.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
/// Return codes of the runtime parse helpers. ABI contract: MUST
/// equal `koja-runtime`'s `parse_text::{PARSE_OK, PARSE_OUT_OF_RANGE}`
/// (invalid format is the switch's default arm). See
/// `koja/design/ABI.md` § Numeric parse helpers.
const PARSE_OK: u64 = 1;
const PARSE_OUT_OF_RANGE: u64 = 2;

pub(super) fn emit_parse<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    target: ParseTarget,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let result_symbol = expect_enum_symbol(&function.return_type, function)?;
    let input_ptr = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "{} missing `input` param on `{}`",
            label(target),
            function.symbol,
        ))
    })?;

    let (helper, out_ty, ok_load_ty): (
        FunctionValue<'ctx>,
        BasicValueEnum<'ctx>,
        inkwell::types::BasicTypeEnum<'ctx>,
    ) = match target {
        ParseTarget::Int => {
            let i64_ty = ctx.context.i64_type();
            let alloca = ctx.build_entry_alloca(i64_ty, "out").into();
            (
                declare_int_parse_extern(ctx),
                alloca,
                i64_ty.as_basic_type_enum(),
            )
        }
        ParseTarget::Float => {
            let f64_ty = ctx.context.f64_type();
            let alloca = ctx.build_entry_alloca(f64_ty, "out").into();
            (
                declare_float_parse_extern(ctx),
                alloca,
                f64_ty.as_basic_type_enum(),
            )
        }
    };

    let i64_ty = ctx.context.i64_type();
    let code = ctx
        .call_basic(helper, &[input_ptr.into(), out_ty.into()], "parse_code")?
        .into_int_value();

    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let out_of_range_bb = ctx
        .context
        .append_basic_block(llvm_function, "out_of_range");
    let invalid_bb = ctx.context.append_basic_block(llvm_function, "invalid");
    ctx.builder
        .build_switch(
            code,
            invalid_bb,
            &[
                (i64_ty.const_int(PARSE_OK, false), ok_bb),
                (i64_ty.const_int(PARSE_OUT_OF_RANGE, false), out_of_range_bb),
            ],
        )
        .or_ice()?;

    emit_ok_branch(
        ctx,
        ok_bb,
        ok_load_ty,
        out_ty.into_pointer_value(),
        result_symbol,
    )?;
    emit_err_branch(ctx, out_of_range_bb, "OutOfRange", result_symbol)?;
    emit_err_branch(ctx, invalid_bb, "InvalidFormat", result_symbol)?;

    Ok(())
}

fn emit_ok_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: BasicBlock<'ctx>,
    load_ty: inkwell::types::BasicTypeEnum<'ctx>,
    out_ptr: PointerValue<'ctx>,
    result_symbol: &IRSymbol,
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let parsed = ctx
        .builder
        .build_load(load_ty, out_ptr, "parsed_val")
        .or_ice()?;
    let ok = build_enum_value(ctx, result_symbol, RESULT_OK_TAG, &[parsed])?;
    ctx.builder.build_return(Some(&ok)).or_ice().map(|_| ())
}

fn emit_err_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    block: BasicBlock<'ctx>,
    variant: &str,
    result_symbol: &IRSymbol,
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let err = build_conversion_error(ctx, result_symbol, variant)?;
    ctx.builder.build_return(Some(&err)).or_ice().map(|_| ())
}

fn expect_enum_symbol<'ty>(
    ty: &'ty IRType,
    function: &IRFunction,
) -> Result<&'ty IRSymbol, LlvmError> {
    match ty {
        IRType::Enum(symbol) => Ok(symbol),
        other => Err(LlvmError::Codegen(format!(
            "parse intrinsic on `{}` expected an enum-typed return, got `{other:?}`",
            function.symbol,
        ))),
    }
}

fn label(target: ParseTarget) -> &'static str {
    match target {
        ParseTarget::Float => "Float.parse",
        ParseTarget::Int => "Int.parse",
    }
}
