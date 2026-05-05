//! Pure operator math: take already-resolved [`Value`] operands and
//! return the [`Value`] produced by an [`IRBinOp`] / [`IRUnaryOp`].
//! No frame, no IR walking, no resolver — every input is concrete.
//!
//! Errors surface as [`RuntimeError::TypeMismatch`] (mismatched
//! widths / shapes — guarded against by typecheck but kept defensive
//! here) and the arithmetic overflow / division-by-zero variants.
//!
//! The dispatcher [`apply_binary_op`] fans out to one helper per
//! operator family (integer arithmetic, boolean logic, equality,
//! integer comparison) so each helper stays exhaustive over the
//! operators it owns and panics with `unreachable!` if the dispatch
//! ever drifts.

use expo_alpha_ir::{IRBinOp, IRUnaryOp};

use crate::error::RuntimeError;
use crate::value::Value;

pub(crate) fn apply_binary_op(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    match op {
        IRBinOp::Add | IRBinOp::Div | IRBinOp::Mod | IRBinOp::Mul | IRBinOp::Sub => {
            apply_int_arith(op, lhs, rhs)
        }
        IRBinOp::And | IRBinOp::Or => apply_bool_logic(op, lhs, rhs),
        IRBinOp::Eq | IRBinOp::NotEq => apply_equality(op, lhs, rhs),
        IRBinOp::Gt | IRBinOp::GtEq | IRBinOp::Lt | IRBinOp::LtEq => {
            apply_int_compare(op, lhs, rhs)
        }
    }
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
        (Value::Int(a), Value::Int(b)) => a == b,
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

fn require_ints(op: IRBinOp, lhs: &Value, rhs: &Value) -> Result<(i64, i64), RuntimeError> {
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok((*a, *b)),
        _ => Err(RuntimeError::TypeMismatch {
            detail: format!("{op:?} expects two Int operands; got {lhs} and {rhs}"),
        }),
    }
}

pub(crate) fn apply_unary_op(op: IRUnaryOp, operand: Value) -> Result<Value, RuntimeError> {
    match op {
        IRUnaryOp::Neg => match operand {
            Value::Int(n) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or(RuntimeError::UnaryIntegerOverflow { op, operand: n }),
            other => Err(RuntimeError::TypeMismatch {
                detail: format!("unary `-` expects an Int operand; got {other}"),
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
