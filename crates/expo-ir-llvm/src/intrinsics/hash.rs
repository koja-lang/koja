//! `Hash.hash` family — `Bool` and the 8 integer cells zero-extend
//! to `i64` and feed the [SplitMix64] finalizer. `String.hash`
//! reads the payload's bit-length header, walks the byte range, and
//! folds each byte through the FNV-1a recurrence.
//!
//! [SplitMix64]: https://prng.di.unimi.it/splitmix64.c

use expo_alpha_ir::{HashImpl, IRFunction, IRSymbol};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

/// `[i64 bit_length][payload bytes]` — the SSA pointer points at
/// the first payload byte; the bit-length sits 8 bytes before.
const STRING_HEADER_BYTES: u64 = 8;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

pub(super) fn emit_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    impl_: HashImpl,
) -> Result<(), LlvmError> {
    match impl_ {
        HashImpl::Bool | HashImpl::Int(_) => emit_int_hash(ctx, function, llvm_function),
        HashImpl::String => emit_string_hash(ctx, function, llvm_function),
    }
}

fn emit_string_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "String.hash missing payload pointer on `{}`",
            function.symbol,
        ))
    })?;
    let str_ptr: PointerValue<'_> = match raw {
        BasicValueEnum::PointerValue(p) => p,
        other => {
            return Err(LlvmError::Codegen(format!(
                "String.hash expected pointer receiver on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };

    let neg_hdr = i64_ty.const_int(-(STRING_HEADER_BYTES as i64) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, str_ptr, &[neg_hdr], "hdr_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let bit_length = ctx
        .builder
        .build_load(i64_ty, hdr_ptr, "bit_length")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_right_shift for `{}`", function.symbol),
                e,
            )
        })?;

    let header_bb = ctx.context.append_basic_block(llvm_function, "fnv_header");
    let body_bb = ctx.context.append_basic_block(llvm_function, "fnv_body");
    let done_bb = ctx.context.append_basic_block(llvm_function, "fnv_done");

    ctx.builder
        .build_unconditional_branch(header_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_unconditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(header_bb);
    let hash_phi = ctx
        .builder
        .build_phi(i64_ty, "hash")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    let idx_phi = ctx
        .builder
        .build_phi(i64_ty, "idx")
        .map_err(|e| inkwell_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    hash_phi.add_incoming(&[(&i64_ty.const_int(FNV_OFFSET_BASIS, false), entry)]);
    idx_phi.add_incoming(&[(&i64_ty.const_zero(), entry)]);
    let hash = hash_phi.as_basic_value().into_int_value();
    let idx = idx_phi.as_basic_value().into_int_value();

    let at_end = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, idx, byte_count, "at_end")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(at_end, done_bb, body_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_conditional_branch for `{}`", function.symbol),
                e,
            )
        })?;

    ctx.builder.position_at_end(body_bb);
    let byte_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, str_ptr, &[idx], "byte_ptr")
            .map_err(|e| inkwell_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let byte = ctx
        .builder
        .build_load(i8_ty, byte_ptr, "byte")
        .map_err(|e| inkwell_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_int_value();
    let byte_ext = ctx
        .builder
        .build_int_z_extend(byte, i64_ty, "byte_ext")
        .map_err(|e| {
            inkwell_err(
                format_args!("build_int_z_extend for `{}`", function.symbol),
                e,
            )
        })?;
    let xored = ctx
        .builder
        .build_xor(hash, byte_ext, "xor_byte")
        .map_err(|e| inkwell_err(format_args!("build_xor for `{}`", function.symbol), e))?;
    let hashed = ctx
        .builder
        .build_int_mul(xored, i64_ty.const_int(FNV_PRIME, false), "fnv_mul")
        .map_err(|e| inkwell_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let next_idx = ctx
        .builder
        .build_int_add(idx, i64_ty.const_int(1, false), "next_idx")
        .map_err(|e| inkwell_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    ctx.builder
        .build_unconditional_branch(header_bb)
        .map_err(|e| {
            inkwell_err(
                format_args!("build_unconditional_branch for `{}`", function.symbol),
                e,
            )
        })?;
    hash_phi.add_incoming(&[(&hashed, body_bb)]);
    idx_phi.add_incoming(&[(&next_idx, body_bb)]);

    ctx.builder.position_at_end(done_bb);
    ctx.builder
        .build_return(Some(&hash))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

fn emit_int_hash<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
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
