//! Eval handlers for the `Equality` intrinsic family — `Bool`, the
//! 8 integer cells (flattened to [`Value::Int(i64)`]), and `String`.
//! Each variant inspects its operands directly; mismatched shapes
//! surface a typed [`RuntimeError::TypeMismatch`] instead of
//! coercing.

use expo_alpha_ir::EqualityImpl;

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
