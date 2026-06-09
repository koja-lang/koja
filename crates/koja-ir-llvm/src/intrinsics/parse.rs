//! `Int.parse(input: String) -> Result<Int, String>` and
//! `Float.parse(input: String) -> Result<Float, String>`.
//!
//! Both delegate to runtime helpers (`koja_int_parse` /
//! `koja_float_parse`) that take an Koja string payload pointer and
//! an out-pointer for the parsed scalar, returning `1` on success
//! and `0` on failure. The intrinsic body allocates an entry-block
//! out slot, calls the helper, branches on the return code, and
//! wraps the parsed value into `Result.Ok(_)` or a literal
//! `"invalid integer"` / `"invalid float"` message into
//! `Result.Err(String)`.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use koja_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, ParseTarget};

use crate::ctx::EmitContext;
use crate::emit::constants::emit_string_literal_payload;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_float_parse_extern, declare_int_parse_extern};

/// `enum Result<T, E>` variant tag for `Ok(T)` — declaration order
/// in `koja/lib/global/src/kernel.koja`.
const RESULT_OK_TAG: IRVariantTag = IRVariantTag(0);
/// `enum Result<T, E>` variant tag for `Err(E)`.
const RESULT_ERR_TAG: IRVariantTag = IRVariantTag(1);

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

    let (helper, out_ty, ok_load_ty, err_message): (
        FunctionValue<'ctx>,
        BasicValueEnum<'ctx>,
        inkwell::types::BasicTypeEnum<'ctx>,
        &[u8],
    ) = match target {
        ParseTarget::Int => {
            let i64_ty = ctx.context.i64_type();
            let alloca = ctx.build_entry_alloca(i64_ty, "out").into();
            (
                declare_int_parse_extern(ctx),
                alloca,
                i64_ty.as_basic_type_enum(),
                b"invalid integer",
            )
        }
        ParseTarget::Float => {
            let f64_ty = ctx.context.f64_type();
            let alloca = ctx.build_entry_alloca(f64_ty, "out").into();
            (
                declare_float_parse_extern(ctx),
                alloca,
                f64_ty.as_basic_type_enum(),
                b"invalid float",
            )
        }
    };

    let i64_ty = ctx.context.i64_type();
    let ok_int = ctx
        .call_basic(helper, &[input_ptr.into(), out_ty.into()], "parsed_ok")?
        .into_int_value();
    let succeeded = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            ok_int,
            i64_ty.const_int(0, false),
            "succeeded",
        )
        .or_ice()?;

    let ok_bb = ctx.context.append_basic_block(llvm_function, "ok");
    let err_bb = ctx.context.append_basic_block(llvm_function, "err");
    ctx.builder
        .build_conditional_branch(succeeded, ok_bb, err_bb)
        .or_ice()?;

    emit_ok_branch(
        ctx,
        ok_bb,
        ok_load_ty,
        out_ty.into_pointer_value(),
        result_symbol,
    )?;
    emit_err_branch(ctx, err_bb, err_message, result_symbol)?;

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
    message: &[u8],
    result_symbol: &IRSymbol,
) -> Result<(), LlvmError> {
    ctx.builder.position_at_end(block);
    let err_msg = emit_string_literal_payload(ctx, message, "parse_err");
    let err = build_enum_value(ctx, result_symbol, RESULT_ERR_TAG, &[err_msg.into()])?;
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
