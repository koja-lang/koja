//! Eval handlers for the 9-cell `Equality` intrinsic family
//! (`Bool.eq` plus `IntN.eq` / `UIntN.eq`).
//!
//! Eval flattens every integer width to [`Value::Int(i64)`], so the
//! Int family collapses to a single `lhs == rhs` comparison; Bool is
//! handled in the same arm because the variants are
//! [`Value::Bool`](crate::Value::Bool) — both shapes round-trip through
//! the same equality check on their underlying primitive.

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
    let result = match (lhs, rhs) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!(
                    "Equality.eq ({impl_:?}) expects matching Bool/Int operands; \
                     got {lhs:?} and {rhs:?}",
                ),
            });
        }
    };
    Ok(Value::Bool(result))
}
