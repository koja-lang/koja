//! `Binary.*` and `Bits.*` intrinsic family. Both layouts share the
//! `[i64 bit_length][ceil(bit_length / 8) bytes]` heap shape; the
//! returned pointer points at the payload, with the bit-length
//! header at offset `-8`. Conversions between `Binary` and `Bits`
//! are zero-cost when the lengths line up.
//!
//! - `Binary.byte_size(self) -> Int` — divides the header by 8.
//! - `Binary.ptr(self) -> CPtr<UInt8>` — returns `self` (the payload
//!   pointer is already byte-addressable).
//! - `Binary.to_bits(self) -> Bits` — zero-cost reinterpret.
//! - `Binary.to_string(self) -> Result<String, String>` — TODO once
//!   UTF-8 validation lands; for now panics via `unreachable`.
//! - `Bits.to_binary(self) -> Result<Binary, String>` — TODO once
//!   the byte-aligned check lands; same placeholder.

use expo_alpha_ir::IRFunction;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

pub(super) fn method_for(id: &str) -> Option<&str> {
    matches!(
        id,
        "Binary.byte_size"
            | "Binary.ptr"
            | "Binary.to_bits"
            | "Binary.to_string"
            | "Bits.to_binary"
    )
    .then_some(id)
}

pub(super) fn emit_binary<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    id: &str,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    match id {
        "Binary.byte_size" => emit_byte_size(ctx, function, llvm_function),
        "Binary.ptr" | "Binary.to_bits" => emit_self_passthrough(ctx, function, llvm_function),
        "Binary.to_string" | "Bits.to_binary" => emit_unimplemented_result(ctx, function),
        other => panic!("emit_binary: unhandled id `{other}`"),
    }
}

/// `byte_size = bit_length / 8`. Reads the i64 header at
/// `payload_ptr - 8`, shifts right by 3 (logical), returns Int.
fn emit_byte_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let i8_ty = ctx.context.i8_type();
    let payload = pointer_param(function, llvm_function, 0)?;
    let neg = i64_ty.const_int((-8i64) as u64, true);
    let hdr_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg], "hdr_ptr")
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
    ctx.builder
        .build_return(Some(&byte_count))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// Zero-cost conversion. Both `Binary` and `Bits` lower to the same
/// payload-pointer shape (`ptr` at the LLVM layer); `Binary.ptr`
/// hands back a `CPtr<UInt8>` shaped identically. Just return `self`.
fn emit_self_passthrough<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let payload = pointer_param(function, llvm_function, 0)?;
    ctx.builder
        .build_return(Some(&payload))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("build_return for `{}`", function.symbol), e))
}

/// Stub for the `Result<T, String>`-returning conversions until the
/// real validation logic lands. Today the body just unreachable-traps;
/// a runtime caller hitting it indicates an upstream "we shouldn't
/// have lowered this yet" bug rather than a recoverable error.
fn emit_unimplemented_result<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<(), LlvmError> {
    ctx.builder.build_unreachable().map(|_| ()).map_err(|e| {
        inkwell_err(
            format_args!("build_unreachable for `{}`", function.symbol),
            e,
        )
    })
}

fn pointer_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let raw = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!("missing param #{index} on `{}`", function.symbol))
    })?;
    match raw {
        BasicValueEnum::PointerValue(p) => Ok(p),
        other => Err(LlvmError::Codegen(format!(
            "expected pointer for param #{index} on `{}`, got `{other:?}`",
            function.symbol,
        ))),
    }
}
