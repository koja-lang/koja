//! Heap-leaf payload acquisition for the conversion intrinsics that
//! hand back an owned `String` / `Binary` / `Bits` derived from a
//! borrowed `self`. All three share the
//! `[i64 rc][i64 bit_length][payload bytes][NUL?]` block layout (the
//! SSA pointer addresses the first payload byte, and the header sits
//! before it, see [`crate::emit::heap_layout`]).
//!
//! Two strategies, picked by whether the result block is layout-
//! identical to the source:
//!
//! - [`share_heap_payload`]: rc-acquire the source's immutable block
//!   and reinterpret the *same* payload pointer as the result type.
//!   The cheap default: Koja blocks are immutable and value semantics
//!   makes the sharing invisible, so a same-layout reinterpret
//!   (`Binary` ↔ `Bits`, `String` -> `Binary`) is just an `rc++`.
//!   Mirrors [`crate::emit::clone`]'s heap-leaf arm.
//! - [`copy_heap_payload`]: deep-copy the header + payload into a
//!   fresh `rc = 1` block. Required only when the result block differs
//!   from the source: `String`'s trailing libc NUL means
//!   `Binary.to_string` and `CString` mint a distinct allocation. This
//!   is also the building block for the eventual "copy on process
//!   boundary" work, where a value must be physically duplicated
//!   across an isolation boundary rather than rc-shared.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_alloc_size, block_base, init_heap_block, load_bit_length};
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::{declare_malloc_extern, declare_rc_inc_extern};

/// rc-acquire `src_payload`'s block and return the same payload
/// pointer, an owned share of the immutable block, no copy. For
/// conversions whose source and result use the identical
/// `[i64 rc][i64 bit_length][payload]` layout, so reinterpreting the
/// pointer is sound: the matching `Drop` rc-decrements either alias
/// and the block is freed once the last owner releases it. The block
/// base (rc word) is `payload - HEADER_BYTES`. Immortal (rodata)
/// blocks are no-ops in the runtime.
pub(super) fn share_heap_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    src_payload: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let base = block_base(ctx, src_payload, &format!("{label}_share_base"))?;
    let rc_inc = declare_rc_inc_extern(ctx);
    ctx.builder
        .build_call(rc_inc, &[base.into()], &format!("{label}_share_inc"))
        .or_ice()?;
    Ok(src_payload)
}

/// Allocate a fresh `[i64 rc][i64 bit_length][payload bytes][NUL?]`
/// block, `memcpy` the payload from `src_payload`, optionally append a
/// trailing NUL, and return the **payload** pointer of the new block
/// (matching the rest-of-pipeline convention where heap values are
/// addressed at their first payload byte). The fresh block gets its
/// own `rc = 1`. The source stays untouched.
///
/// Callers wire it into a wider control-flow shape when they want
/// `Result.Ok(new_payload)` (`Binary.to_string`) vs a plain return.
pub(crate) fn copy_heap_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    src_payload: PointerValue<'ctx>,
    with_nul: bool,
    ceil_byte_count: bool,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let three = i64_ty.const_int(3, false);

    // The source's `bit_length` (at `payload - LENGTH_OFFSET`). The
    // fresh block gets its own `rc = 1`, never the source's count.
    let bit_length = load_bit_length(ctx, src_payload, "copy_src")?;
    let byte_count = byte_count_from_bits(ctx, label, bit_length, ceil_byte_count, three)?;
    let alloc_size = block_alloc_size(ctx, byte_count, with_nul, "alloc_size")?;

    let malloc = declare_malloc_extern(ctx);
    let dst_base = ctx
        .call_basic(malloc, &[alloc_size.into()], "copy_buf")?
        .into_pointer_value();
    let dst_payload = init_heap_block(ctx, dst_base, bit_length, "copy_dst")?;

    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[dst_payload.into(), src_payload.into(), byte_count.into()],
            "",
        )
        .or_ice()?;

    if with_nul {
        let nul = unsafe {
            ctx.builder
                .build_in_bounds_gep(i8_ty, dst_payload, &[byte_count], "nul")
                .or_ice()?
        };
        ctx.builder.build_store(nul, i8_ty.const_zero()).or_ice()?;
    }

    Ok(dst_payload)
}

fn byte_count_from_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    _label: &str,
    bit_length: IntValue<'ctx>,
    ceil: bool,
    three: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    if ceil {
        let i64_ty = ctx.context.i64_type();
        let bits_plus7 = ctx
            .builder
            .build_int_add(bit_length, i64_ty.const_int(7, false), "bits_plus7")
            .or_ice()?;
        ctx.builder
            .build_right_shift(bits_plus7, three, false, "byte_count")
            .or_ice()
    } else {
        ctx.builder
            .build_right_shift(bit_length, three, false, "byte_count")
            .or_ice()
    }
}

/// Fetch param 0 (`self`) as the payload pointer for a heap-leaf
/// receiver. Surfaces a codegen error if the slot isn't a pointer.
pub(super) fn pointer_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "heap-leaf intrinsic missing `self` payload pointer on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::PointerValue(p) => Ok(p),
        other => Err(LlvmError::Codegen(format!(
            "heap-leaf intrinsic expected pointer receiver on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
