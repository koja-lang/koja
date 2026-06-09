//! Bitwise intrinsics for `Global.{Int,Int8..32,UInt8..64}`. Six
//! ops × eight integer types = 48 intrinsic ids dispatched here.
//! All bodies are pure LLVM IR — `build_{and,or,xor,not}` for the
//! logical ops; `build_left_shift` / `build_right_shift` for the
//! shifts (right shift uses the signed flag pulled from the
//! receiver's type so signed types arithmetic-shift and unsigned
//! types logical-shift).
//!
//! Shift count parameters are typed `Int` in source, which means
//! `Int64` at the IR layer. LLVM requires the count to match the
//! operand's int type, so the emitter truncates the count down to
//! the operand width before issuing the shift. Truncation is safe:
//! the typecheck doesn't reject negative or wide-magnitude shifts
//! today, but LLVM's shift-by-N≥width is poison anyway, and the
//! truncate just collapses the same poison into the operand's
//! native shape.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{FunctionValue, IntValue};
use koja_ir::{BitOp, IRFunction, IntType};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::types::ir_basic_type;

pub(super) fn emit_bitwise<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    ty: IntType,
    op: BitOp,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);

    let lhs = receiver_param(function, llvm_function);
    let result: IntValue<'ctx> = match op {
        BitOp::Band => {
            let rhs = other_param(function, llvm_function);
            ctx.builder.build_and(lhs, rhs, "band").or_ice()?
        }
        BitOp::Bnot => ctx.builder.build_not(lhs, "bnot").or_ice()?,
        BitOp::Bor => {
            let rhs = other_param(function, llvm_function);
            ctx.builder.build_or(lhs, rhs, "bor").or_ice()?
        }
        BitOp::Bsl => {
            let count = shift_count(ctx, function, llvm_function, &lhs)?;
            ctx.builder.build_left_shift(lhs, count, "bsl").or_ice()?
        }
        BitOp::Bsr => {
            let count = shift_count(ctx, function, llvm_function, &lhs)?;
            ctx.builder
                .build_right_shift(lhs, count, ty.is_signed(), "bsr")
                .or_ice()?
        }
        BitOp::Bxor => {
            let rhs = other_param(function, llvm_function);
            ctx.builder.build_xor(lhs, rhs, "bxor").or_ice()?
        }
    };

    ctx.builder.build_return(Some(&result)).or_ice().map(|_| ())
}

/// Read the receiver (`self`) param, the first positional parameter
/// of every `Bitwise` method.
fn receiver_param<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> IntValue<'ctx> {
    llvm_function
        .get_nth_param(0)
        .unwrap_or_else(|| {
            panic!(
                "bitwise intrinsic `{}` missing receiver param",
                function.symbol
            )
        })
        .into_int_value()
}

/// Read the second positional param — `other` for the binary ops
/// (`band`/`bor`/`bxor`).
fn other_param<'ctx>(function: &IRFunction, llvm_function: FunctionValue<'ctx>) -> IntValue<'ctx> {
    llvm_function
        .get_nth_param(1)
        .unwrap_or_else(|| {
            panic!(
                "bitwise intrinsic `{}` missing `other` param",
                function.symbol
            )
        })
        .into_int_value()
}

/// Mint a same-width shift-count for `bsl` / `bsr`. The source
/// signature types `n: Int` (Int64); LLVM requires the count to
/// match the operand's int type, so we truncate down to the
/// receiver's native width. No-op when the receiver is already
/// `Int64`.
fn shift_count<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    receiver: &IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let raw = llvm_function
        .get_nth_param(1)
        .unwrap_or_else(|| panic!("bitwise shift `{}` missing `n` param", function.symbol))
        .into_int_value();
    if raw.get_type() == receiver.get_type() {
        return Ok(raw);
    }
    let target = match ir_basic_type(ctx, &function.params[0].ty)? {
        BasicTypeEnum::IntType(int_ty) => int_ty,
        _ => unreachable!(
            "bitwise shift `{}`: receiver param is non-int after typecheck",
            function.symbol,
        ),
    };
    ctx.builder
        .build_int_truncate(raw, target, "shift_count")
        .or_ice()
}
