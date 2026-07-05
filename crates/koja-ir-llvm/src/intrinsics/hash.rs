//! `Hash.hash` family: `Bool` and the 8 integer cells zero-extend
//! to `i64` and feed the [SplitMix64] finalizer. `String.hash`
//! reads the payload's bit-length header, walks the byte range, and
//! folds each byte through the FNV-1a recurrence.
//!
//! [SplitMix64]: https://prng.di.unimi.it/splitmix64.c

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{HashImpl, IRFunction, IRSymbol};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::load_bit_length;
use crate::error::{IceExt, LlvmError};

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

    let bit_length = load_bit_length(ctx, str_ptr, "bit_length")?;
    let byte_count = ctx
        .builder
        .build_right_shift(bit_length, i64_ty.const_int(3, false), false, "byte_count")
        .or_ice()?;

    let header_bb = ctx.context.append_basic_block(llvm_function, "fnv_header");
    let body_bb = ctx.context.append_basic_block(llvm_function, "fnv_body");
    let done_bb = ctx.context.append_basic_block(llvm_function, "fnv_done");

    ctx.builder.build_unconditional_branch(header_bb).or_ice()?;

    ctx.builder.position_at_end(header_bb);
    let hash_phi = ctx.builder.build_phi(i64_ty, "hash").or_ice()?;
    let idx_phi = ctx.builder.build_phi(i64_ty, "idx").or_ice()?;
    hash_phi.add_incoming(&[(&i64_ty.const_int(FNV_OFFSET_BASIS, false), entry)]);
    idx_phi.add_incoming(&[(&i64_ty.const_zero(), entry)]);
    let hash = hash_phi.as_basic_value().into_int_value();
    let idx = idx_phi.as_basic_value().into_int_value();

    let at_end = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, idx, byte_count, "at_end")
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(at_end, done_bb, body_bb)
        .or_ice()?;

    ctx.builder.position_at_end(body_bb);
    let byte_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, str_ptr, &[idx], "byte_ptr")
            .or_ice()?
    };
    let byte = ctx
        .builder
        .build_load(i8_ty, byte_ptr, "byte")
        .or_ice()?
        .into_int_value();
    let byte_ext = ctx
        .builder
        .build_int_z_extend(byte, i64_ty, "byte_ext")
        .or_ice()?;
    let xored = ctx.builder.build_xor(hash, byte_ext, "xor_byte").or_ice()?;
    let hashed = ctx
        .builder
        .build_int_mul(xored, i64_ty.const_int(FNV_PRIME, false), "fnv_mul")
        .or_ice()?;
    let next_idx = ctx
        .builder
        .build_int_add(idx, i64_ty.const_int(1, false), "next_idx")
        .or_ice()?;
    ctx.builder.build_unconditional_branch(header_bb).or_ice()?;
    hash_phi.add_incoming(&[(&hashed, body_bb)]);
    idx_phi.add_incoming(&[(&next_idx, body_bb)]);

    ctx.builder.position_at_end(done_bb);
    ctx.builder.build_return(Some(&hash)).or_ice().map(|_| ())
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
            .or_ice()?
    } else {
        value
    };
    let mixed = splitmix64(ctx, &function.symbol, extended)?;
    ctx.builder.build_return(Some(&mixed)).or_ice().map(|_| ())
}

/// SplitMix64 finalizer. Three rounds of `(x ^= x >> shift) *= odd
/// constant`. The exact constants match every other Koja backend so
/// hash values are byte-stable across eval / native / future JIT.
fn splitmix64<'ctx>(
    ctx: &EmitContext<'ctx>,
    _symbol: &IRSymbol,
    value: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let shr30 = ctx
        .builder
        .build_right_shift(value, i64_ty.const_int(30, false), false, "shr30")
        .or_ice()?;
    let xor1 = ctx.builder.build_xor(value, shr30, "xor1").or_ice()?;
    let mul1 = ctx
        .builder
        .build_int_mul(xor1, i64_ty.const_int(0xbf58_476d_1ce4_e5b9, false), "mul1")
        .or_ice()?;
    let shr27 = ctx
        .builder
        .build_right_shift(mul1, i64_ty.const_int(27, false), false, "shr27")
        .or_ice()?;
    let xor2 = ctx.builder.build_xor(mul1, shr27, "xor2").or_ice()?;
    let mul2 = ctx
        .builder
        .build_int_mul(xor2, i64_ty.const_int(0x94d0_49bb_1331_11eb, false), "mul2")
        .or_ice()?;
    let shr31 = ctx
        .builder
        .build_right_shift(mul2, i64_ty.const_int(31, false), false, "shr31")
        .or_ice()?;
    ctx.builder.build_xor(mul2, shr31, "xor3").or_ice()
}
