//! `Concat` emission. `String` and `Binary` byte-align and inline a
//! `malloc + memcpy + memcpy + (NUL)` shape; `Bits` defers to the
//! `__koja_concat_bits` runtime helper because sub-byte
//! alignment is far cleaner in Rust than LLVM IR.

use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use koja_ir::ConcatKind;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::runtime::{declare_concat_bits_extern, declare_malloc_extern};

use super::inkwell_err;

/// Lower an `IRInstruction::Concat` to its per-kind shape. `String`
/// and `Binary` both byte-align — the common shape is `malloc(8 +
/// total_bytes [+1])` + two `memcpy`s + (String only) trailing
/// `\0`. `Bits` defers to the `__koja_concat_bits` runtime
/// helper.
pub(super) fn emit_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    kind: ConcatKind,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    match kind {
        ConcatKind::Bits => {
            let helper = declare_concat_bits_extern(ctx);
            let result = ctx
                .builder
                .build_call(helper, &[lhs.into(), rhs.into()], "concat_bits")
                .map_err(|e| inkwell_err(format_args!("concat_bits call"), e))?;
            let basic = result.try_as_basic_value().basic().ok_or_else(|| {
                LlvmError::Codegen(
                    "LLVM emit: __koja_concat_bits returned void; \
                     runtime declaration drift?"
                        .to_string(),
                )
            })?;
            Ok(basic)
        }
        ConcatKind::String | ConcatKind::Binary => {
            emit_byte_aligned_concat(ctx, lhs, rhs, matches!(kind, ConcatKind::String))
        }
    }
}

/// `String` / `Binary` share a single inline shape: load both
/// `i64 bit_length`s from the `payload-8` headers, derive byte
/// counts via `>> 3`, `malloc` the combined block, store the
/// combined `bit_length`, `memcpy` lhs then rhs payloads, and (for
/// `String`) write a trailing `\0`. Returns the new payload pointer.
fn emit_byte_aligned_concat<'ctx>(
    ctx: &EmitContext<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    with_nul: bool,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let neg8 = i64_ty.const_int((-8i64) as u64, true);
    let eight = i64_ty.const_int(8, false);
    let three = i64_ty.const_int(3, false);
    let l_ptr = lhs.into_pointer_value();
    let r_ptr = rhs.into_pointer_value();

    let (l_bits, l_bytes) = load_bit_length(ctx, l_ptr, "l", i8_ty, i64_ty, neg8, three)?;
    let (r_bits, r_bytes) = load_bit_length(ctx, r_ptr, "r", i8_ty, i64_ty, neg8, three)?;

    let total_bits = ctx
        .builder
        .build_int_add(l_bits, r_bits, "cat_total_bits")
        .map_err(|e| inkwell_err(format_args!("concat total_bits"), e))?;
    let total_bytes = ctx
        .builder
        .build_int_add(l_bytes, r_bytes, "cat_total_bytes")
        .map_err(|e| inkwell_err(format_args!("concat total_bytes"), e))?;
    let header_size = if with_nul {
        i64_ty.const_int(9, false)
    } else {
        eight
    };
    let alloc_size = ctx
        .builder
        .build_int_add(total_bytes, header_size, "cat_alloc")
        .map_err(|e| inkwell_err(format_args!("concat alloc"), e))?;

    let malloc = declare_malloc_extern(ctx);
    let base = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "cat_base")
        .map_err(|e| inkwell_err(format_args!("concat malloc"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("malloc returned void".to_string()))?
        .into_pointer_value();

    ctx.builder
        .build_store(base, total_bits)
        .map_err(|e| inkwell_err(format_args!("concat store header"), e))?;

    let payload = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, base, &[eight], "cat_payload")
            .map_err(|e| inkwell_err(format_args!("concat payload GEP"), e))?
    };

    ctx.builder
        .build_memcpy(payload, 1, l_ptr, 1, l_bytes)
        .map_err(|e| inkwell_err(format_args!("concat memcpy lhs"), e))?;

    let mid = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, payload, &[l_bytes], "cat_mid")
            .map_err(|e| inkwell_err(format_args!("concat mid GEP"), e))?
    };
    ctx.builder
        .build_memcpy(mid, 1, r_ptr, 1, r_bytes)
        .map_err(|e| inkwell_err(format_args!("concat memcpy rhs"), e))?;

    if with_nul {
        let end = unsafe {
            ctx.builder
                .build_in_bounds_gep(i8_ty, payload, &[total_bytes], "cat_end")
                .map_err(|e| inkwell_err(format_args!("concat end GEP"), e))?
        };
        ctx.builder
            .build_store(end, i8_ty.const_int(0, false))
            .map_err(|e| inkwell_err(format_args!("concat NUL store"), e))?;
    }

    Ok(payload.into())
}

/// Load the `i64 bit_length` header at `payload - 8` plus its
/// derived `bit_length >> 3` byte count. Shared between the lhs /
/// rhs sides of [`emit_byte_aligned_concat`]; `prefix` is just for
/// LLVM SSA-name readability.
fn load_bit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    prefix: &str,
    i8_ty: inkwell::types::IntType<'ctx>,
    i64_ty: inkwell::types::IntType<'ctx>,
    neg8: IntValue<'ctx>,
    three: IntValue<'ctx>,
) -> Result<(IntValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let hdr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg8], &format!("{prefix}_hdr"))
            .map_err(|e| inkwell_err(format_args!("concat header GEP for `{prefix}`"), e))?
    };
    let bits = ctx
        .builder
        .build_load(i64_ty, hdr, &format!("{prefix}_bits"))
        .map_err(|e| inkwell_err(format_args!("concat header load for `{prefix}`"), e))?
        .into_int_value();
    let bytes = ctx
        .builder
        .build_right_shift(bits, three, false, &format!("{prefix}_bytes"))
        .map_err(|e| inkwell_err(format_args!("concat byte count for `{prefix}`"), e))?;
    Ok((bits, bytes))
}
