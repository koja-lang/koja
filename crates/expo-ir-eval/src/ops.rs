//! Pure binary and unary operator evaluators for the
//! [`crate::Interp`] backend.
//!
//! All entry points take owned [`Value`]s and return a new [`Value`]
//! (or [`RuntimeError::TypeMismatch`] on shape errors). They never
//! consult interpreter state, so they live as free functions.

use expo_ir::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};

use crate::error::RuntimeError;
use crate::value::Value;

/// Evaluate a [`ResolvedBinaryOp`] against two materialized operands.
pub(crate) fn eval_binary_op(
    op: &ResolvedBinaryOp,
    lhs: Value,
    rhs: Value,
) -> Result<Value, RuntimeError> {
    use ResolvedBinaryOp::*;
    match op {
        BoolAnd => bool_op(lhs, rhs, |a, b| a && b),
        BoolOr => bool_op(lhs, rhs, |a, b| a || b),
        EnumStructEqual { negated } => {
            let eq = lhs == rhs;
            Ok(Value::Bool(if *negated { !eq } else { eq }))
        }
        FloatAdd => float_arith(lhs, rhs, |a, b| a + b),
        FloatDiv => float_arith(lhs, rhs, |a, b| a / b),
        FloatEqual => float_compare(lhs, rhs, |a, b| a == b),
        FloatGreater => float_compare(lhs, rhs, |a, b| a > b),
        FloatGreaterEqual => float_compare(lhs, rhs, |a, b| a >= b),
        FloatLess => float_compare(lhs, rhs, |a, b| a < b),
        FloatLessEqual => float_compare(lhs, rhs, |a, b| a <= b),
        FloatMul => float_arith(lhs, rhs, |a, b| a * b),
        FloatNotEqual => float_compare(lhs, rhs, |a, b| a != b),
        FloatRem => float_arith(lhs, rhs, |a, b| a % b),
        FloatSub => float_arith(lhs, rhs, |a, b| a - b),
        IntAdd => int_arith(lhs, rhs, |a, b| a.wrapping_add(b)),
        IntDiv => int_arith(lhs, rhs, |a, b| a.wrapping_div(b)),
        IntEqual => int_compare(lhs, rhs, |a, b| a == b),
        IntGreater => int_compare(lhs, rhs, |a, b| a > b),
        IntGreaterEqual => int_compare(lhs, rhs, |a, b| a >= b),
        IntLess => int_compare(lhs, rhs, |a, b| a < b),
        IntLessEqual => int_compare(lhs, rhs, |a, b| a <= b),
        IntMul => int_arith(lhs, rhs, |a, b| a.wrapping_mul(b)),
        IntNotEqual => int_compare(lhs, rhs, |a, b| a != b),
        IntRem => int_arith(lhs, rhs, |a, b| a.wrapping_rem(b)),
        IntSub => int_arith(lhs, rhs, |a, b| a.wrapping_sub(b)),
        StringEqual => string_compare(lhs, rhs, |a, b| a == b),
        StringNotEqual => string_compare(lhs, rhs, |a, b| a != b),
    }
}

/// Evaluate a [`ResolvedUnaryOp`] against a materialized operand.
pub(crate) fn eval_unary_op(op: &ResolvedUnaryOp, operand: Value) -> Result<Value, RuntimeError> {
    match op {
        ResolvedUnaryOp::FloatNeg => match operand {
            Value::Float(x) => Ok(Value::Float(-x)),
            Value::Float32(x) => Ok(Value::Float32(-x)),
            other => Err(RuntimeError::TypeMismatch(format!(
                "FloatNeg expects float, got {other:?}"
            ))),
        },
        ResolvedUnaryOp::IntNeg => {
            let i = operand.as_int().ok_or_else(|| {
                RuntimeError::TypeMismatch(format!("IntNeg expects int, got {operand:?}"))
            })?;
            Ok(Value::Int(-i))
        }
        ResolvedUnaryOp::IntNot => {
            let i = operand.as_int().ok_or_else(|| {
                RuntimeError::TypeMismatch(format!("IntNot expects int, got {operand:?}"))
            })?;
            Ok(Value::Int(!i))
        }
    }
}

fn bool_op(lhs: Value, rhs: Value, op: fn(bool, bool) -> bool) -> Result<Value, RuntimeError> {
    let a = lhs
        .as_bool()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected bool, got {lhs:?}")))?;
    let b = rhs
        .as_bool()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected bool, got {rhs:?}")))?;
    Ok(Value::Bool(op(a, b)))
}

fn both_floats(lhs: Value, rhs: Value) -> Result<(f64, f64), RuntimeError> {
    let a = match lhs {
        Value::Float(x) => x,
        Value::Float32(x) => x as f64,
        other => {
            return Err(RuntimeError::TypeMismatch(format!(
                "expected float, got {other:?}"
            )));
        }
    };
    let b = match rhs {
        Value::Float(x) => x,
        Value::Float32(x) => x as f64,
        other => {
            return Err(RuntimeError::TypeMismatch(format!(
                "expected float, got {other:?}"
            )));
        }
    };
    Ok((a, b))
}

fn float_arith(lhs: Value, rhs: Value, op: fn(f64, f64) -> f64) -> Result<Value, RuntimeError> {
    let (a, b) = both_floats(lhs, rhs)?;
    Ok(Value::Float(op(a, b)))
}

fn float_compare(lhs: Value, rhs: Value, op: fn(f64, f64) -> bool) -> Result<Value, RuntimeError> {
    let (a, b) = both_floats(lhs, rhs)?;
    Ok(Value::Bool(op(a, b)))
}

fn int_arith(lhs: Value, rhs: Value, op: fn(i64, i64) -> i64) -> Result<Value, RuntimeError> {
    let a = lhs
        .as_int()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected int, got {lhs:?}")))?;
    let b = rhs
        .as_int()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected int, got {rhs:?}")))?;
    Ok(Value::Int(op(a, b)))
}

fn int_compare(lhs: Value, rhs: Value, op: fn(i64, i64) -> bool) -> Result<Value, RuntimeError> {
    let a = lhs
        .as_int()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected int, got {lhs:?}")))?;
    let b = rhs
        .as_int()
        .ok_or_else(|| RuntimeError::TypeMismatch(format!("expected int, got {rhs:?}")))?;
    Ok(Value::Bool(op(a, b)))
}

fn string_compare(
    lhs: Value,
    rhs: Value,
    op: fn(&str, &str) -> bool,
) -> Result<Value, RuntimeError> {
    let (Value::String(a), Value::String(b)) = (&lhs, &rhs) else {
        return Err(RuntimeError::TypeMismatch(format!(
            "expected strings, got {lhs:?} / {rhs:?}"
        )));
    };
    Ok(Value::Bool(op(a.as_str(), b.as_str())))
}
