//! Eval handlers for the `Equality` intrinsic family — `Bool`,
//! 8 integer cells (flattened to [`Value::Int(i64)`]), `Float` /
//! `Float32` (IEEE-754 ordered: `NaN != NaN`), and `String`.
//! Mismatched shapes surface a typed
//! [`RuntimeError::TypeMismatch`] instead of coercing.

use koja_ir::{EqualityImpl, FloatType};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(impl_: EqualityImpl, args: &[Value]) -> Result<Value, RuntimeError> {
    let [lhs, rhs] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Equality.eq ({impl_:?}) expects 2 arguments; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let result = match (impl_, lhs, rhs) {
        (EqualityImpl::Bool, Value::Bool(a), Value::Bool(b)) => a == b,
        (EqualityImpl::Int(_), Value::Int(a), Value::Int(b)) => a == b,
        (EqualityImpl::Float(FloatType::Float), Value::Float64(a), Value::Float64(b)) => a == b,
        (EqualityImpl::Float(FloatType::Float32), Value::Float32(a), Value::Float32(b)) => a == b,
        (EqualityImpl::String, Value::String(a), Value::String(b)) => a == b,
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!(
                    "Equality.eq ({impl_:?}) expects matching operands for the impl cell; \
                     got {lhs:?} and {rhs:?}",
                ),
            });
        }
    };
    Ok(Value::Bool(result))
}
