//! `Debug.format` family — primitive `format(self) -> String`
//! intrinsics. Each receiver shape (`Int`/`IntN`/`UIntN`, `Float` /
//! `Float32`, `Bool`) routes to a single runtime helper
//! (`koja_format_*`) that returns a freshly-allocated Koja string
//! payload. The auto-print wrapper goes through the same helpers
//! so backend output stays byte-exact with the eval interpreter.
//!
//! Signed vs. unsigned widening: signed receivers (`Int`/`IntN`)
//! sign-extend to `i64` and route through `koja_format_i64`;
//! unsigned (`UIntN`) zero-extend to `i64` (`u64` ABI-wise) and
//! route through `koja_format_u64`. `Bool` zero-extends through
//! the same `i64`-shaped helper as the auto-print wrapper.
//!
//! `String.format` is intentionally absent — it ships a pure-Koja
//! body in `lib/global/src/debug.koja`, not an intrinsic.

use inkwell::values::{BasicValueEnum, FunctionValue};
use koja_ir::{DebugImpl, IRFunction, IntType};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{
    FORMAT_BOOL_SYMBOL, FORMAT_F32_SYMBOL, FORMAT_F64_SYMBOL, FORMAT_I64_SYMBOL, FORMAT_U64_SYMBOL,
    declare_runtime_format,
};

pub(super) fn emit_format<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    impl_: DebugImpl,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "Debug.format missing `self` param on `{}`",
            function.symbol,
        ))
    })?;
    let payload = match impl_ {
        DebugImpl::Bool => format_via_i64(ctx, function, raw, FORMAT_BOOL_SYMBOL)?,
        DebugImpl::Float => format_via_f64(ctx, function, raw)?,
        DebugImpl::Float32 => format_via_f32(ctx, function, raw)?,
        DebugImpl::Int(ty) => format_via_int(ctx, function, raw, ty)?,
    };
    ctx.builder
        .build_return(Some(&payload))
        .map(|_| ())
        .map_err(|e| {
            inkwell_err(
                format_args!("Debug.format build_return on `{}`", function.symbol),
                e,
            )
        })
}

/// `Bool` widens through `koja_format_bool(i64)`. The shared
/// `i64` helper signature lets us reuse [`format_via_i64`] for
/// both `Bool` (zero-extended `i1`) and any other future non-int
/// receiver that funnels through the boolean path.
fn format_via_i64<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    raw: BasicValueEnum<'ctx>,
    symbol: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let int_value = match raw {
        BasicValueEnum::IntValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Debug.format on `{}` expected int param, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let i64_ty = ctx.context.i64_type();
    let widened = ctx
        .builder
        .build_int_z_extend(int_value, i64_ty, "fmt.zext")
        .map_err(|e| {
            inkwell_err(
                format_args!("Debug.format zext on `{}`", function.symbol),
                e,
            )
        })?;
    let helper = declare_runtime_format(ctx, symbol, i64_ty.into());
    call_format_helper(ctx, function, helper, widened.into(), symbol)
}

fn format_via_f32<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    raw: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let float_value = match raw {
        BasicValueEnum::FloatValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Float32.format on `{}` expected float param, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let f32_ty = ctx.context.f32_type();
    let helper = declare_runtime_format(ctx, FORMAT_F32_SYMBOL, f32_ty.into());
    call_format_helper(ctx, function, helper, float_value.into(), FORMAT_F32_SYMBOL)
}

fn format_via_f64<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    raw: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let float_value = match raw {
        BasicValueEnum::FloatValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Float.format on `{}` expected float param, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let f64_ty = ctx.context.f64_type();
    let helper = declare_runtime_format(ctx, FORMAT_F64_SYMBOL, f64_ty.into());
    call_format_helper(ctx, function, helper, float_value.into(), FORMAT_F64_SYMBOL)
}

/// Sign- or zero-extend the integer receiver to the matching
/// 64-bit ABI and route through `koja_format_i64` /
/// `koja_format_u64`. The runtime side renders signed and unsigned
/// values differently; the LLVM emitter picks the helper from
/// [`IntType::is_signed`] so the rendered bytes match each
/// receiver's source-level signedness.
fn format_via_int<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    raw: BasicValueEnum<'ctx>,
    ty: IntType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let int_value = match raw {
        BasicValueEnum::IntValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Debug.format on `{}` expected int param, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let i64_ty = ctx.context.i64_type();
    let widened = if ty.is_signed() {
        ctx.builder
            .build_int_s_extend(int_value, i64_ty, "fmt.sext")
            .map_err(|e| {
                inkwell_err(
                    format_args!("Debug.format sext on `{}`", function.symbol),
                    e,
                )
            })?
    } else {
        ctx.builder
            .build_int_z_extend(int_value, i64_ty, "fmt.zext")
            .map_err(|e| {
                inkwell_err(
                    format_args!("Debug.format zext on `{}`", function.symbol),
                    e,
                )
            })?
    };
    let symbol = if ty.is_signed() {
        FORMAT_I64_SYMBOL
    } else {
        FORMAT_U64_SYMBOL
    };
    let helper = declare_runtime_format(ctx, symbol, i64_ty.into());
    call_format_helper(ctx, function, helper, widened.into(), symbol)
}

fn call_format_helper<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    helper: FunctionValue<'ctx>,
    arg: inkwell::values::BasicMetadataValueEnum<'ctx>,
    symbol: &str,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let call_site = ctx
        .builder
        .build_call(helper, &[arg], "fmt.call")
        .map_err(|e| {
            inkwell_err(
                format_args!("Debug.format build_call {symbol} on `{}`", function.symbol),
                e,
            )
        })?;
    call_site.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "{symbol} returned no value on `{}`",
            function.symbol,
        ))
    })
}
