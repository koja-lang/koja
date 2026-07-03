//! `Concat` emission. `String` and `Binary` byte-align and inline a
//! `malloc + memcpy + memcpy + (NUL)` shape. `Bits` defers to the
//! `__koja_concat_bits` runtime helper because sub-byte
//! alignment is far cleaner in Rust than LLVM IR.

use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use koja_ir::ConcatKind;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_concat_bits_extern, declare_malloc_extern};

use super::heap_layout::{block_alloc_size, init_heap_block, load_bit_length};

/// Lower an `IRInstruction::Concat` to its per-kind shape. `String`
/// and `Binary` both byte-align: the common shape is `malloc(8 +
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
            ctx.call_basic(helper, &[lhs.into(), rhs.into()], "concat_bits")
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
    let three = i64_ty.const_int(3, false);
    let l_ptr = lhs.into_pointer_value();
    let r_ptr = rhs.into_pointer_value();

    let (l_bits, l_bytes) = bits_and_bytes(ctx, l_ptr, "l", three)?;
    let (r_bits, r_bytes) = bits_and_bytes(ctx, r_ptr, "r", three)?;

    let total_bits = ctx
        .builder
        .build_int_add(l_bits, r_bits, "cat_total_bits")
        .or_ice()?;
    let total_bytes = ctx
        .builder
        .build_int_add(l_bytes, r_bytes, "cat_total_bytes")
        .or_ice()?;
    let alloc_size = block_alloc_size(ctx, total_bytes, with_nul, "cat_alloc")?;

    let malloc = declare_malloc_extern(ctx);
    let base = ctx
        .call_basic(malloc, &[alloc_size.into()], "cat_base")?
        .into_pointer_value();

    let payload = init_heap_block(ctx, base, total_bits, "cat")?;

    ctx.builder
        .build_memcpy(payload, 1, l_ptr, 1, l_bytes)
        .or_ice()?;

    let mid = unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, payload, &[l_bytes], "cat_mid")
            .or_ice()?
    };
    ctx.builder
        .build_memcpy(mid, 1, r_ptr, 1, r_bytes)
        .or_ice()?;

    if with_nul {
        let end = unsafe {
            ctx.builder
                .build_in_bounds_gep(i8_ty, payload, &[total_bytes], "cat_end")
                .or_ice()?
        };
        ctx.builder
            .build_store(end, i8_ty.const_int(0, false))
            .or_ice()?;
    }

    Ok(payload.into())
}

/// Load a heap payload's `i64 bit_length` (the word at `payload -
/// LENGTH_OFFSET`) plus its derived `bit_length >> 3` byte count.
/// Shared between the lhs / rhs sides of [`emit_byte_aligned_concat`].
/// `prefix` is just for LLVM SSA-name readability.
fn bits_and_bytes<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    prefix: &str,
    three: IntValue<'ctx>,
) -> Result<(IntValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let bits = load_bit_length(ctx, payload, &format!("{prefix}_bits"))?;
    let bytes = ctx
        .builder
        .build_right_shift(bits, three, false, &format!("{prefix}_bytes"))
        .or_ice()?;
    Ok((bits, bytes))
}
