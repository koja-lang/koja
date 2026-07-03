//! `Equality.eq` family. `Bool` + the 8 integer cells share an
//! `icmp eq` emitter (eval flattens both to fixed-width integers).
//! `Float` / `Float32` use `fcmp oeq` (ordered: `NaN == NaN` is
//! false, matching IEEE 754 and source-level `==`). `String.eq`
//! delegates to libc `strcmp`. String payloads carry a
//! trailing NUL, so byte-sequence equality matches Koja's source-
//! level `==` semantics.

use inkwell::values::{BasicValueEnum, FloatValue, FunctionValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};
use koja_ir::{EqualityImpl, IRFunction};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::declare_strcmp_extern;

pub(super) fn emit_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    impl_: EqualityImpl,
) -> Result<(), LlvmError> {
    match impl_ {
        EqualityImpl::Bool | EqualityImpl::Int(_) => emit_int_eq(ctx, function, llvm_function),
        EqualityImpl::Float(_) => emit_float_eq(ctx, function, llvm_function),
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
        .call_basic(strcmp, &[lhs.into(), rhs.into()], "strcmp")?
        .into_int_value();
    let cmp = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            diff,
            ctx.context.i32_type().const_zero(),
            "streq",
        )
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
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
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
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

/// Ordered IEEE 754 equality: `NaN` operands always return `false`,
/// matching source-level `f == f`. Float32 / Float64 share the
/// emitter, and LLVM picks the width from the param's actual type.
fn emit_float_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let lhs = nth_float(function, llvm_function, 0, "self")?;
    let rhs = nth_float(function, llvm_function, 1, "other")?;
    let cmp = ctx
        .builder
        .build_float_compare(FloatPredicate::OEQ, lhs, rhs, "feq")
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
}

fn nth_float<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> Result<FloatValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "missing param `{name}` (#{index}) on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::FloatValue(v) => Ok(v),
        other => Err(LlvmError::Codegen(format!(
            "expected float for `{name}` on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
