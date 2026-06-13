//! Pure operator math: take already-resolved [`Value`] operands and
//! return the [`Value`] produced by an [`IRBinOp`] / [`IRUnaryOp`].
//! No frame, no IR walking, no resolver — every input is concrete.
//!
//! Errors surface as [`RuntimeError::TypeMismatch`] (mismatched
//! widths / shapes — guarded against by typecheck but kept defensive
//! here) and the arithmetic overflow / division-by-zero variants.
//!
//! The dispatcher [`apply_binary_op`] fans out to one helper per
//! operator family + numeric shape (integer / float arithmetic,
//! integer / float comparison, boolean logic, equality) so each
//! helper stays exhaustive over the operators it owns and panics
//! with `unreachable!` if the dispatch ever drifts. Numeric shape
//! (`Int` vs `Float32` / `Float64`) is decided at the
//! `apply_binary_op` seam by peeking the operand variants — typecheck
//! guarantees both sides agree, including width.

use std::ops::{Add, Div, Mul, Rem, Sub};

use koja_ir::{IRBinOp, IRUnaryOp};

use crate::error::RuntimeError;
use crate::value::Value;

pub(crate) fn apply_binary_op(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    match op {
        IRBinOp::Add | IRBinOp::Div | IRBinOp::Mod | IRBinOp::Mul | IRBinOp::Sub => {
            if is_float(&lhs) || is_float(&rhs) {
                apply_float_arith(op, lhs, rhs)
            } else {
                apply_int_arith(op, lhs, rhs)
            }
        }
        IRBinOp::And | IRBinOp::Or => apply_bool_logic(op, lhs, rhs),
        IRBinOp::Eq | IRBinOp::NotEq => apply_equality(op, lhs, rhs),
        IRBinOp::Gt | IRBinOp::GtEq | IRBinOp::Lt | IRBinOp::LtEq => {
            if is_float(&lhs) || is_float(&rhs) {
                apply_float_compare(op, lhs, rhs)
            } else {
                apply_int_compare(op, lhs, rhs)
            }
        }
    }
}

fn is_float(value: &Value) -> bool {
    matches!(value, Value::Float32(_) | Value::Float64(_))
}

fn apply_int_arith(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let (a, b) = require_ints(op, &lhs, &rhs)?;
    let checked = match op {
        IRBinOp::Add => a.checked_add(b),
        IRBinOp::Div => {
            if b == 0 {
                return Err(RuntimeError::DivisionByZero { op });
            }
            a.checked_div(b)
        }
        IRBinOp::Mod => {
            if b == 0 {
                return Err(RuntimeError::DivisionByZero { op });
            }
            a.checked_rem(b)
        }
        IRBinOp::Mul => a.checked_mul(b),
        IRBinOp::Sub => a.checked_sub(b),
        _ => unreachable!("apply_int_arith dispatched with non-arith op {op:?}"),
    };
    checked
        .map(Value::Int)
        .ok_or(RuntimeError::IntegerOverflow { lhs: a, op, rhs: b })
}

fn apply_bool_logic(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let (Value::Bool(a), Value::Bool(b)) = (&lhs, &rhs) else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("{op:?} expects two Bool operands; got {lhs} and {rhs}"),
        });
    };
    let result = match op {
        IRBinOp::And => *a && *b,
        IRBinOp::Or => *a || *b,
        _ => unreachable!("apply_bool_logic dispatched with non-logic op {op:?}"),
    };
    Ok(Value::Bool(result))
}

fn apply_equality(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let equal = match (&lhs, &rhs) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        // IEEE 754: `NaN == NaN` is false; `Float64::partial_cmp`
        // routes through the `==` operator on `f64` which already
        // honours that. Same for `Float32`.
        (Value::Float32(a), Value::Float32(b)) => a == b,
        (Value::Float64(a), Value::Float64(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Unit, Value::Unit) => true,
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("{op:?} requires operands of the same type; got {lhs} and {rhs}"),
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

fn apply_int_compare(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let (a, b) = require_ints(op, &lhs, &rhs)?;
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
/// match the LLVM backend's native `float` / `double` ops. Diverges
/// from [`apply_int_arith`] in two IEEE-754-flavored ways: division /
/// modulo by zero produce `inf` / `NaN` (no `DivisionByZero` raise),
/// and overflow saturates to `±inf` (no `Overflow` raise).
fn apply_float_arith(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    Ok(match require_floats(op, &lhs, &rhs)? {
        Floats::F32(a, b) => Value::Float32(float_arith(op, a, b)),
        Floats::F64(a, b) => Value::Float64(float_arith(op, a, b)),
    })
}

/// Float comparisons. `NaN` on either side returns `false` from
/// every ordered predicate (matches LLVM's `OEQ`/`OLT`/etc., which
/// are the predicates emit-side picks).
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
            detail: format!("{op:?} expects two Int operands; got {lhs} and {rhs}"),
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
                "{op:?} expects two Float operands of the same width; got {lhs} and {rhs}"
            ),
        }),
    }
}

pub(crate) fn apply_unary_op(op: IRUnaryOp, operand: Value) -> Result<Value, RuntimeError> {
    match op {
        IRUnaryOp::Neg => match operand {
            // IEEE 754 negation never traps (every float has a
            // representable negative); diverges from the int arm's
            // `i64::MIN` overflow check.
            Value::Float32(v) => Ok(Value::Float32(-v)),
            Value::Float64(v) => Ok(Value::Float64(-v)),
            Value::Int(n) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or(RuntimeError::UnaryIntegerOverflow { op, operand: n }),
            other => Err(RuntimeError::TypeMismatch {
                detail: format!("unary `-` expects an Int or Float operand; got {other}"),
            }),
        },
        IRUnaryOp::Not => match operand {
            Value::Bool(b) => Ok(Value::Bool(!b)),
            other => Err(RuntimeError::TypeMismatch {
                detail: format!("`not` expects a Bool operand; got {other}"),
            }),
        },
    }
}
