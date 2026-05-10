//! `Hash.hash` family — one impl per primitive integer / bool type.
//! Every cell zero-extends the receiver to `i64` and runs the
//! [SplitMix64] finalizer; output is `Int` (= `i64`). `Bool` follows
//! the same path with a 1-bit zero-extend. Mirrors v1's
//! `expo_codegen::intrinsics::hash::emit_splitmix64` shape so eval
//! and native produce identical hash values.
//!
//! [SplitMix64]: https://prng.di.unimi.it/splitmix64.c

use expo_alpha_ir::{HashImpl, IRFunction, IRSymbol};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

pub(super) fn emit_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    _impl_: HashImpl,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let i64_ty = ctx.context.i64_type();
    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!("missing receiver param on `{}`", function.symbol))
    })?;
    let value = match raw {
        BasicValueEnum::IntValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "expected integer receiver on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let extended = if value.get_type().get_bit_width() < 64 {
        ctx.builder
            .build_int_z_extend(value, i64_ty, "ext")
            .map_err(|e| {
                inkwell_err(
                    format_args!("build_int_z_extend for `{}`", function.symbol),
                    e,
                )
            })?
    } else {
        value
    };
    let mixed = splitmix64(ctx, &function.symbol, extended)?;
    ctx.builder
        .build_return(Some(&mixed))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// SplitMix64 finalizer. Three rounds of `(x ^= x >> shift) *= odd
/// constant`; the exact constants match every other Expo backend so
/// hash values are byte-stable across eval / native / future JIT.
fn splitmix64<'ctx>(
    ctx: &EmitContext<'ctx>,
    symbol: &IRSymbol,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let shr30 = ctx
        .builder
        .build_right_shift(value, i64_ty.const_int(30, false), false, "shr30")
        .map_err(|e| inkwell_err(format_args!("build_right_shift for `{symbol}`"), e))?;
    let xor1 = ctx
        .builder
        .build_xor(value, shr30, "xor1")
        .map_err(|e| inkwell_err(format_args!("build_xor for `{symbol}`"), e))?;
    let mul1 = ctx
        .builder
        .build_int_mul(xor1, i64_ty.const_int(0xbf58_476d_1ce4_e5b9, false), "mul1")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{symbol}`"), e))?;
    let shr27 = ctx
        .builder
        .build_right_shift(mul1, i64_ty.const_int(27, false), false, "shr27")
        .map_err(|e| inkwell_err(format_args!("build_right_shift for `{symbol}`"), e))?;
    let xor2 = ctx
        .builder
        .build_xor(mul1, shr27, "xor2")
        .map_err(|e| inkwell_err(format_args!("build_xor for `{symbol}`"), e))?;
    let mul2 = ctx
        .builder
        .build_int_mul(xor2, i64_ty.const_int(0x94d0_49bb_1331_11eb, false), "mul2")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{symbol}`"), e))?;
    let shr31 = ctx
        .builder
        .build_right_shift(mul2, i64_ty.const_int(31, false), false, "shr31")
        .map_err(|e| inkwell_err(format_args!("build_right_shift for `{symbol}`"), e))?;
    ctx.builder
        .build_xor(mul2, shr31, "xor3")
        .map_err(|e| inkwell_err(format_args!("build_xor for `{symbol}`"), e))
}
