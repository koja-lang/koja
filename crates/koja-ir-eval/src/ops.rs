//! Pure operator math: take already-resolved [`Value`] operands and
//! return the [`Value`] produced by an [`IRBinOp`] / [`IRUnaryOp`].
//! No frame, no IR walking, no resolver: every input is concrete.
//!
//! The instruction's `operand_ty` drives numeric shape. Integer
//! arithmetic runs at the operand type's width and signedness (in
//! `i128`, then range-checked back), and comparisons pick signed or
//! unsigned ordering from it. Arithmetic faults (overflow, zero
//! divisors, `MIN / -1`, non-finite float results) surface as
//! [`RuntimeError::Panicked`] with the shared `koja_ir` message
//! constants, matching the LLVM backend's `__koja_panic` output
//! verbatim.
//!
//! Type mismatches (guarded against by typecheck but kept defensive
//! here) surface as [`RuntimeError::TypeMismatch`].

use std::ops::{Add, Div, Mul, Rem, Sub};

use koja_ir::{BinarySign, IRBinOp, IRType, IRUnaryOp, NEG_OVERFLOW_MESSAGE};

use crate::error::RuntimeError;
use crate::value::Value;

pub(crate) fn apply_binary_op(
    op: IRBinOp,
    operand_ty: &IRType,
    lhs: Value,
    rhs: Value,
) -> Result<Value, RuntimeError> {
    match op {
        IRBinOp::Add | IRBinOp::Div | IRBinOp::Mod | IRBinOp::Mul | IRBinOp::Sub => {
            if operand_ty.is_float() {
                apply_float_arith(op, lhs, rhs)
            } else {
                apply_int_arith(op, operand_ty, lhs, rhs)
            }
        }
        IRBinOp::Eq | IRBinOp::NotEq => apply_equality(op, lhs, rhs),
        IRBinOp::Gt | IRBinOp::GtEq | IRBinOp::Lt | IRBinOp::LtEq => {
            if operand_ty.is_float() {
                apply_float_compare(op, lhs, rhs)
            } else {
                apply_int_compare(op, operand_ty, lhs, rhs)
            }
        }
    }
}

/// Inclusive value range of the integer type, computed from width +
/// signedness. `Value::Int` stores every width in an `i64` (unsigned
/// 64-bit values bit-preserved), so faults are detected by exact
/// `i128` arithmetic against this range.
fn int_range(ty: &IRType) -> (i128, i128) {
    let width = ty.int_bit_width().unwrap_or(64);
    match ty.int_sign() {
        Some(BinarySign::Unsigned) => (0, (1i128 << width) - 1),
        _ => (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1),
    }
}

/// The operand's mathematical value. Signed operands read the stored
/// `i64` directly, unsigned operands reinterpret its bit pattern
/// (`UInt64` values above `i64::MAX` are stored as negative `i64`).
fn int_operand(ty: &IRType, stored: i64) -> i128 {
    match ty.int_sign() {
        Some(BinarySign::Unsigned) => (stored as u64) as i128,
        _ => stored as i128,
    }
}

fn apply_int_arith(
    op: IRBinOp,
    ty: &IRType,
    lhs: Value,
    rhs: Value,
) -> Result<Value, RuntimeError> {
    let (a, b) = require_ints(op, &lhs, &rhs)?;
    let (a, b) = (int_operand(ty, a), int_operand(ty, b));
    if matches!(op, IRBinOp::Div | IRBinOp::Mod) && b == 0 {
        return Err(RuntimeError::Panicked {
            message: op.division_by_zero_message(),
        });
    }
    let exact = match op {
        IRBinOp::Add => a + b,
        IRBinOp::Div => a / b,
        IRBinOp::Mod => a % b,
        IRBinOp::Mul => a * b,
        IRBinOp::Sub => a - b,
        _ => unreachable!("apply_int_arith dispatched with non-arith op {op:?}"),
    };
    let (min, max) = int_range(ty);
    if exact < min || exact > max {
        return Err(RuntimeError::Panicked {
            message: op.overflow_message(),
        });
    }
    Ok(Value::Int(exact as u64 as i64))
}

fn apply_equality(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let equal = match (&lhs, &rhs) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Float32(a), Value::Float32(b)) => a == b,
        (Value::Float64(a), Value::Float64(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("{op:?} requires operands of the same type, got {lhs} and {rhs}"),
            });
        }
    };
    let result = match op {
        IRBinOp::Eq => equal,
        IRBinOp::NotEq => !equal,
        _ => unreachable!("apply_equality dispatched with non-equality op {op:?}"),
    };
    Ok(Value::Bool(result))
}

fn apply_int_compare(
    op: IRBinOp,
    ty: &IRType,
    lhs: Value,
    rhs: Value,
) -> Result<Value, RuntimeError> {
    let (a, b) = require_ints(op, &lhs, &rhs)?;
    let (a, b) = (int_operand(ty, a), int_operand(ty, b));
    let result = match op {
        IRBinOp::Gt => a > b,
        IRBinOp::GtEq => a >= b,
        IRBinOp::Lt => a < b,
        IRBinOp::LtEq => a <= b,
        _ => unreachable!("apply_int_compare dispatched with non-compare op {op:?}"),
    };
    Ok(Value::Bool(result))
}

/// Float arithmetic, computed at the operands' own width so results
/// match the LLVM backend's native `float` / `double` ops. A
/// non-finite IEEE result (overflow to ±inf, `0.0 / 0.0`, …) traps,
/// upholding the finite-only `Float` invariant.
fn apply_float_arith(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let result = match require_floats(op, &lhs, &rhs)? {
        Floats::F32(a, b) => {
            let v = float_arith(op, a, b);
            v.is_finite().then_some(Value::Float32(v))
        }
        Floats::F64(a, b) => {
            let v = float_arith(op, a, b);
            v.is_finite().then_some(Value::Float64(v))
        }
    };
    result.ok_or_else(|| RuntimeError::Panicked {
        message: op.non_finite_message(),
    })
}

/// Float comparisons. With NaN unrepresentable, ordered comparison
/// is total (matches LLVM's `OEQ`/`OLT`/etc. predicates emit-side).
fn apply_float_compare(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    Ok(Value::Bool(match require_floats(op, &lhs, &rhs)? {
        Floats::F32(a, b) => float_compare(op, a, b),
        Floats::F64(a, b) => float_compare(op, a, b),
    }))
}

fn float_arith<F>(op: IRBinOp, a: F, b: F) -> F
where
    F: Add<Output = F> + Div<Output = F> + Mul<Output = F> + Rem<Output = F> + Sub<Output = F>,
{
    match op {
        IRBinOp::Add => a + b,
        IRBinOp::Div => a / b,
        IRBinOp::Mod => a % b,
        IRBinOp::Mul => a * b,
        IRBinOp::Sub => a - b,
        _ => unreachable!("float_arith dispatched with non-arith op {op:?}"),
    }
}

fn float_compare<F: PartialOrd>(op: IRBinOp, a: F, b: F) -> bool {
    match op {
        IRBinOp::Gt => a > b,
        IRBinOp::GtEq => a >= b,
        IRBinOp::Lt => a < b,
        IRBinOp::LtEq => a <= b,
        _ => unreachable!("float_compare dispatched with non-compare op {op:?}"),
    }
}

fn require_ints(op: IRBinOp, lhs: &Value, rhs: &Value) -> Result<(i64, i64), RuntimeError> {
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok((*a, *b)),
        _ => Err(RuntimeError::TypeMismatch {
            detail: format!("{op:?} expects two Int operands, got {lhs} and {rhs}"),
        }),
    }
}

/// Two float operands of matching width. Typecheck rejects mixed-width
/// arithmetic, so the cross-width pairings are unreachable.
enum Floats {
    F32(f32, f32),
    F64(f64, f64),
}

fn require_floats(op: IRBinOp, lhs: &Value, rhs: &Value) -> Result<Floats, RuntimeError> {
    match (lhs, rhs) {
        (Value::Float32(a), Value::Float32(b)) => Ok(Floats::F32(*a, *b)),
        (Value::Float64(a), Value::Float64(b)) => Ok(Floats::F64(*a, *b)),
        _ => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "{op:?} expects two Float operands of the same width, got {lhs} and {rhs}"
            ),
        }),
    }
}

pub(crate) fn apply_unary_op(
    op: IRUnaryOp,
    operand_ty: &IRType,
    operand: Value,
) -> Result<Value, RuntimeError> {
    match op {
        IRUnaryOp::Neg => match operand {
            // Float negation never traps. Every finite float has a
            // representable negative.
            Value::Float32(v) => Ok(Value::Float32(-v)),
            Value::Float64(v) => Ok(Value::Float64(-v)),
            Value::Int(n) => {
                let negated = -(n as i128);
                let (min, max) = int_range(operand_ty);
                if negated < min || negated > max {
                    return Err(RuntimeError::Panicked {
                        message: NEG_OVERFLOW_MESSAGE.to_string(),
                    });
                }
                Ok(Value::Int(negated as i64))
            }
            other => Err(RuntimeError::TypeMismatch {
                detail: format!("unary `-` expects an Int or Float operand, got {other}"),
            }),
        },
        IRUnaryOp::Not => match operand {
            Value::Bool(b) => Ok(Value::Bool(!b)),
            other => Err(RuntimeError::TypeMismatch {
                detail: format!("`not` expects a Bool operand, got {other}"),
            }),
        },
    }
}
