//! Eval handlers for the 9-cell `Equality` intrinsic family
//! (`Bool.eq`, `Int.eq`, `Int8.eq`, `Int16.eq`, `Int32.eq`,
//! `UInt8.eq`, `UInt16.eq`, `UInt32.eq`, `UInt64.eq`).
//!
//! Eval flattens every integer width to [`Value::Int(i64)`], so the
//! Int family collapses to a single `lhs == rhs` comparison; Bool is
//! handled separately because the variants are
//! [`Value::Bool`](crate::Value::Bool).

use crate::error::RuntimeError;
use crate::value::Value;

const INT_PREFIXES: &[&str] = &[
    "Int.", "Int8.", "Int16.", "Int32.", "UInt8.", "UInt16.", "UInt32.", "UInt64.",
];

pub(super) fn matches_id(id: &str) -> bool {
    if id == "Bool.eq" {
        return true;
    }
    INT_PREFIXES
        .iter()
        .any(|prefix| id.strip_prefix(prefix) == Some("eq"))
}

pub(super) fn dispatch(id: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    let [lhs, rhs] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "`{id}` expects 2 arguments; got {} arg(s): {args:?}",
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
                    "`{id}` expects matching Bool/Int operands; got {lhs:?} and {rhs:?}"
                ),
            });
        }
    };
    Ok(Value::Bool(result))
}
