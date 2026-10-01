//! Eval handlers for the 48-cell `Bitwise` intrinsic family.
//!
//! AND/OR/XOR operate directly on the stored `i64`. `bnot` and the
//! shifts are computed at the receiver's width ([`IntType`] carries
//! it), with results masked and re-extended to match the LLVM
//! backend's native narrow-int instructions. Shift counts outside
//! `0 <= n < width` trap with the shared
//! [`BitOp::shift_count_message`] panic.

use koja_ir::{BitOp, IntType};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

/// Run a bitwise intrinsic. `ty` selects shift signedness and the
/// width used for count validation and result normalization.
pub(super) fn dispatch(ty: IntType, op: BitOp, args: &[Value]) -> Result<Value, RuntimeError> {
    let label = label(op);
    let lhs = helpers::arg_int(args, 0, label)?;
    let result = match op {
        BitOp::Band => lhs & helpers::arg_int(args, 1, label)?,
        BitOp::Bnot => normalize(ty, !lhs),
        BitOp::Bor => lhs | helpers::arg_int(args, 1, label)?,
        BitOp::Bsl => {
            let count = shift_count(ty, op, helpers::arg_int(args, 1, label)?)?;
            normalize(ty, lhs.wrapping_shl(count))
        }
        BitOp::Bsr => {
            let count = shift_count(ty, op, helpers::arg_int(args, 1, label)?)?;
            if ty.is_signed() {
                lhs.wrapping_shr(count)
            } else {
                normalize(ty, ((lhs as u64).wrapping_shr(count)) as i64)
            }
        }
        BitOp::Bxor => lhs ^ helpers::arg_int(args, 1, label)?,
    };
    Ok(Value::Int(result))
}

/// The `Bitwise.<method>` label for argument diagnostics.
fn label(op: BitOp) -> &'static str {
    match op {
        BitOp::Band => "Bitwise.band",
        BitOp::Bnot => "Bitwise.bnot",
        BitOp::Bor => "Bitwise.bor",
        BitOp::Bsl => "Bitwise.bsl",
        BitOp::Bsr => "Bitwise.bsr",
        BitOp::Bxor => "Bitwise.bxor",
    }
}

/// Validate a shift count against the receiver width, trapping on
/// negative or width-and-larger counts like the LLVM backend.
fn shift_count(ty: IntType, op: BitOp, count: i64) -> Result<u32, RuntimeError> {
    if count < 0 || count >= ty.bit_width() as i64 {
        return Err(RuntimeError::Panicked {
            message: op.shift_count_message().to_string(),
        });
    }
    Ok(count as u32)
}

/// Re-establish the `Value::Int` storage convention after an op that
/// can spill past the receiver's width. Masks to `width` bits, then
/// sign-extends signed receivers (unsigned stay zero-extended).
fn normalize(ty: IntType, value: i64) -> i64 {
    let width = ty.bit_width();
    if width == 64 {
        return value;
    }
    let masked = (value as u64) & ((1u64 << width) - 1);
    if ty.is_signed() && masked >> (width - 1) == 1 {
        (masked as i64) - (1i64 << width)
    } else {
        masked as i64
    }
}
