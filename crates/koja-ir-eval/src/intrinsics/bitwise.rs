//! Eval handlers for the 48-cell `Bitwise` intrinsic family.
//!
//! Eval collapses every integer width (`Int{8,16,32,64}` /
//! `UInt{8,16,32,64}`) to [`Value::Int(i64)`](crate::Value::Int),
//! so the AND/OR/XOR ops are width-agnostic at this layer and
//! operate directly on i64. `bnot` and the shifts are computed at
//! the receiver's width ([`IntType`] carries it), with results
//! masked and re-extended to match the LLVM backend's native
//! narrow-int instructions. Shift counts outside `0 <= n < width`
//! trap with the shared [`BitOp::shift_count_message`] panic.

use koja_ir::{BitOp, IntType};

use crate::error::RuntimeError;
use crate::value::Value;

/// Run a bitwise intrinsic. `ty` selects shift signedness and the
/// width used for count validation and result normalization.
pub(super) fn dispatch(ty: IntType, op: BitOp, args: &[Value]) -> Result<Value, RuntimeError> {
    let lhs = arg_int(args, 0, op)?;
    let result = match op {
        BitOp::Band => lhs & arg_int(args, 1, op)?,
        BitOp::Bnot => normalize(ty, !lhs),
        BitOp::Bor => lhs | arg_int(args, 1, op)?,
        BitOp::Bsl => {
            let count = shift_count(ty, op, arg_int(args, 1, op)?)?;
            normalize(ty, lhs.wrapping_shl(count))
        }
        BitOp::Bsr => {
            let count = shift_count(ty, op, arg_int(args, 1, op)?)?;
            if ty.is_signed() {
                lhs.wrapping_shr(count)
            } else {
                normalize(ty, ((lhs as u64).wrapping_shr(count)) as i64)
            }
        }
        BitOp::Bxor => lhs ^ arg_int(args, 1, op)?,
    };
    Ok(Value::Int(result))
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

fn arg_int(args: &[Value], index: usize, op: BitOp) -> Result<i64, RuntimeError> {
    match args.get(index) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeMismatch {
            detail: format!("Bitwise.{op:?} arg #{index}: expected Int, got {other:?}"),
        }),
        None => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bitwise.{op:?} arity: expected at least {expected} args, got {got}",
                expected = index + 1,
                got = args.len(),
            ),
        }),
    }
}
