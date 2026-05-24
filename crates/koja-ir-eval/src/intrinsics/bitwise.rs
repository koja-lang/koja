//! Eval handlers for the 48-cell `Bitwise` intrinsic family.
//!
//! Eval collapses every integer width (`Int{8,16,32,64}` /
//! `UInt{8,16,32,64}`) to [`Value::Int(i64)`](crate::Value::Int),
//! so the AND/OR/XOR/NOT/SHL ops are width-agnostic at this layer
//! and operate directly on i64. Right shift is the only op where
//! the receiver type matters: signed types use arithmetic shift
//! (sign-extend, native `i64 >> n`), unsigned types use logical
//! shift (cast through `u64 >> n`). [`IntType::is_signed`] supplies
//! the answer; the typed dispatch payload threads it through
//! without re-parsing strings.

use koja_ir::{BitOp, IntType};

use crate::error::RuntimeError;
use crate::value::Value;

/// Run a bitwise intrinsic. `ty` selects right-shift signedness;
/// every other op is width-agnostic on the eval `Value::Int(i64)`
/// representation.
pub(super) fn dispatch(ty: IntType, op: BitOp, args: &[Value]) -> Result<Value, RuntimeError> {
    let lhs = arg_int(args, 0, op)?;
    let result = match op {
        BitOp::Band => lhs & arg_int(args, 1, op)?,
        BitOp::Bnot => !lhs,
        BitOp::Bor => lhs | arg_int(args, 1, op)?,
        BitOp::Bsl => {
            let n = arg_int(args, 1, op)?;
            // Cast to u32 for the Rust shift; out-of-range counts
            // are undefined in LLVM and saturate-to-zero in this
            // interpreter (panics worse than mismatched semantics).
            lhs.wrapping_shl(n as u32)
        }
        BitOp::Bsr => {
            let n = arg_int(args, 1, op)?;
            if ty.is_signed() {
                lhs.wrapping_shr(n as u32)
            } else {
                ((lhs as u64).wrapping_shr(n as u32)) as i64
            }
        }
        BitOp::Bxor => lhs ^ arg_int(args, 1, op)?,
    };
    Ok(Value::Int(result))
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
