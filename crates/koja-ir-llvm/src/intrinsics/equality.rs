//! `Equality.equals?` family. `Bool` + the 8 integer cells share an
//! `icmp eq` emitter (eval flattens both to fixed-width integers).
//! `Float` / `Float32` use `fcmp oeq` (ordered: `NaN == NaN` is
//! false, matching IEEE 754 and source-level `==`). `String.eq` and
//! `Binary.eq` share the runtime's length-aware byte comparison,
//! since both types carry the same `[rc][bit_length][bytes]` payload.

use inkwell::values::FunctionValue;
use inkwell::{FloatPredicate, IntPredicate};
use koja_ir::{EqualityImpl, IRFunction};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{nth_float, nth_int, nth_param};
use crate::runtime::declare_string_eq_extern;

pub(super) fn emit_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    impl_: EqualityImpl,
) -> Result<(), LlvmError> {
    match impl_ {
        EqualityImpl::Bool | EqualityImpl::Int(_) => emit_int_eq(ctx, function, llvm_function),
        EqualityImpl::Float(_) => emit_float_eq(ctx, function, llvm_function),
        EqualityImpl::Binary | EqualityImpl::String => emit_bytes_eq(ctx, function, llvm_function),
    }
}

/// Length-plus-byte comparison shared by `String.eq` and
/// `Binary.eq`. Both receivers carry the same payload layout, so the
/// runtime helper serves either.
fn emit_bytes_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let lhs = nth_param(function, llvm_function, 0, "self")?;
    let rhs = nth_param(function, llvm_function, 1, "other")?;
    let string_eq = declare_string_eq_extern(ctx);
    let equal = ctx
        .call_basic(string_eq, &[lhs.into(), rhs.into()], "string_eq")?
        .into_int_value();
    let cmp = ctx
        .builder
        .build_int_compare(
            IntPredicate::NE,
            equal,
            ctx.context.i64_type().const_zero(),
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
    let lhs = nth_int(function, llvm_function, 0, "self")?;
    let rhs = nth_int(function, llvm_function, 1, "other")?;
    let cmp = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq")
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
}

/// Ordered IEEE 754 equality: `NaN` operands always return `false`,
/// matching source-level `f == f`. Float32 / Float64 share the
/// emitter, and LLVM picks the width from the param's actual type.
fn emit_float_eq<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let lhs = nth_float(function, llvm_function, 0, "self")?;
    let rhs = nth_float(function, llvm_function, 1, "other")?;
    let cmp = ctx
        .builder
        .build_float_compare(FloatPredicate::OEQ, lhs, rhs, "feq")
        .or_ice()?;
    ctx.builder.build_return(Some(&cmp)).or_ice().map(|_| ())
}
