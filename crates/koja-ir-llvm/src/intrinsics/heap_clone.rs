//! Shared backbone for the `Clone` intrinsics on `String` /
//! `Binary` / `Bits`. All three share the
//! `[i64 bit_length][ceil(bit_length / 8) bytes]` heap layout (with
//! the SSA pointer at the first payload byte and the `i64` header at
//! offset `-8`); the only per-receiver differences are whether to
//! round the byte count up (`Bits` allows a trailing partial byte)
//! and whether to write a trailing NUL after the payload (`String`
//! does, for libc compat). One helper, three thin call-sites in
//! [`super::string`] / [`super::binary`].
//!
//! Layout chosen as a single `memcpy` of the header word + payload
//! together — the source already has them contiguous and the
//! destination is freshly `malloc`'d, so a one-shot copy beats two
//! independent stores. The trailing `\0` (when requested) is a
//! single byte store after the memcpy.

use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_alloc_size, init_heap_block, load_bit_length};
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::intrinsics::cptr::declare_memcpy_extern;
use crate::runtime::declare_malloc_extern;

/// Emit a `Clone` body that allocates a fresh `[i64 bit_length]
/// [payload bytes]` block, `memcpy`s the header word + payload from
/// the source, optionally writes a trailing NUL, and returns the
/// payload pointer of the new block. The source stays live — `Clone`
/// is `borrow self` — so we never touch the source layout.
///
/// - `with_nul`: write a trailing `\0` past the payload (true for
///   `String`, false for `Binary` / `Bits`).
/// - `ceil_byte_count`: derive the payload byte count from the bit
///   length using `(bits + 7) >> 3` (true for `Bits`, which permits a
///   trailing partial byte) instead of plain `bits >> 3` (true for
///   `String` / `Binary`, which are always byte-aligned).
pub(super) fn emit_payload_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    with_nul: bool,
    ceil_byte_count: bool,
) -> Result<(), LlvmError> {
    let payload = pointer_param(function, llvm_function)?;
    let dst_payload = copy_heap_payload(
        ctx,
        function.symbol.mangled(),
        payload,
        with_nul,
        ceil_byte_count,
    )?;
    ctx.builder
        .build_return(Some(&dst_payload))
        .map(|_| ())
        .map_err(|e| {
            inkwell_err(
                format_args!("clone build_return for `{}`", function.symbol),
                e,
            )
        })
}

/// Allocate a fresh `[i64 bit_length][payload bytes][NUL?]` block,
/// `memcpy` the header + payload from `src_payload`, optionally
/// append a trailing NUL, and return the **payload** pointer of the
/// new block (matching the rest-of-pipeline convention where heap
/// values are addressed at their first payload byte, header at
/// offset `-8`).
///
/// Same shape every heap-clone-flavored intrinsic in the pipeline needs —
/// callers wire it into a wider control-flow shape when they want
/// `Result.Ok(new_payload)` (`Binary.to_string`) vs a plain return
/// (`String.clone`).
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

    // The source's `bit_length` (at `payload - LENGTH_OFFSET`); the
    // fresh block gets its own `rc = 1`, never the source's count.
    let bit_length = load_bit_length(ctx, src_payload, "clone_src")?;
    let byte_count = byte_count_from_bits(ctx, label, bit_length, ceil_byte_count, three)?;
    let alloc_size = block_alloc_size(ctx, byte_count, with_nul, "alloc_size")?;

    let malloc = declare_malloc_extern(ctx);
    let dst_base = ctx
        .builder
        .build_call(malloc, &[alloc_size.into()], "clone_buf")
        .map_err(|e| inkwell_err(format_args!("clone malloc for `{label}`"), e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen(format!("malloc returned no value for `{label}`")))?
        .into_pointer_value();
    let dst_payload = init_heap_block(ctx, dst_base, bit_length, "clone_dst")?;

    let memcpy = declare_memcpy_extern(ctx);
    ctx.builder
        .build_call(
            memcpy,
            &[dst_payload.into(), src_payload.into(), byte_count.into()],
            "",
        )
        .map_err(|e| inkwell_err(format_args!("clone memcpy for `{label}`"), e))?;

    if with_nul {
        let nul = unsafe {
            ctx.builder
                .build_in_bounds_gep(i8_ty, dst_payload, &[byte_count], "nul")
                .map_err(|e| inkwell_err(format_args!("clone NUL GEP for `{label}`"), e))?
        };
        ctx.builder
            .build_store(nul, i8_ty.const_zero())
            .map_err(|e| inkwell_err(format_args!("clone NUL store for `{label}`"), e))?;
    }

    Ok(dst_payload)
}

fn byte_count_from_bits<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    bit_length: IntValue<'ctx>,
    ceil: bool,
    three: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    if ceil {
        let i64_ty = ctx.context.i64_type();
        let bits_plus7 = ctx
            .builder
            .build_int_add(bit_length, i64_ty.const_int(7, false), "bits_plus7")
            .map_err(|e| inkwell_err(format_args!("clone bits+7 for `{label}`"), e))?;
        ctx.builder
            .build_right_shift(bits_plus7, three, false, "byte_count")
            .map_err(|e| inkwell_err(format_args!("clone byte_count for `{label}`"), e))
    } else {
        ctx.builder
            .build_right_shift(bit_length, three, false, "byte_count")
            .map_err(|e| inkwell_err(format_args!("clone byte_count for `{label}`"), e))
    }
}

pub(super) fn pointer_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "clone intrinsic missing `self` payload pointer on `{}`",
            function.symbol,
        ))
    })?;
    match raw {
        BasicValueEnum::PointerValue(p) => Ok(p),
        other => Err(LlvmError::Codegen(format!(
            "clone intrinsic expected pointer receiver on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
