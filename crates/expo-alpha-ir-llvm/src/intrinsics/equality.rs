//! `Equality.eq` family — `Bool` and the 8 integer cells share a
//! single `icmp eq` emitter (eval flattens both shapes to fixed-
//! width integers). `String.eq` delegates to libc `strcmp`; alpha
//! string payloads carry a trailing NUL, so byte-sequence equality
//! matches Expo's source-level `==` semantics.

use expo_alpha_ir::{EqualityImpl, IRFunction};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::declare_strcmp_extern;

pub(super) fn emit_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    impl_: EqualityImpl,
) -> Result<(), LlvmError> {
    match impl_ {
        EqualityImpl::Bool | EqualityImpl::Int(_) => emit_int_eq(ctx, function, llvm_function),
        EqualityImpl::String => emit_string_eq(ctx, function, llvm_function),
    }
}

fn emit_string_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let lhs = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String.eq missing `self` param on `{}`",
            function.symbol,
        ))
    })?;
    let rhs = llvm_function.get_nth_param(1).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String.eq missing `other` param on `{}`",
            function.symbol,
        ))
    })?;
    let strcmp = declare_strcmp_extern(ctx);
    let diff = ctx
        .builder
        .build_call(strcmp, &[lhs.into(), rhs.into()], "strcmp")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_call strcmp for `{}`", function.symbol),
                e,
            )
        })?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "strcmp returned no value for `{}`",
                function.symbol,
            ))
        })?
        .into_int_value();
    let cmp = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            diff,
            ctx.context.i32_type().const_zero(),
            "streq",
        )
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(Some(&cmp))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_int_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let lhs = nth_int(function, llvm_function, 0, "self")?;
    let rhs = nth_int(function, llvm_function, 1, "other")?;
    let cmp = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_return(Some(&cmp))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn nth_int<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing param `{name}` (#{index}) on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::IntValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected integer for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
