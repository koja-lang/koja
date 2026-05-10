//! Eval handlers for the 9-cell `Hash` intrinsic family (`Bool.hash`
//! plus `IntN.hash` / `UIntN.hash`).
//!
//! Implements SplitMix64 against the value re-interpreted as `u64`,
//! matching the LLVM-side [`crate::intrinsics::hash`] emitter so eval /
//! native produce byte-identical hash codes for the same input.

use expo_alpha_ir::HashImpl;

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(impl_: HashImpl, args: &[Value]) -> Result<Value, RuntimeError> {
    let [arg] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Hash.hash ({impl_:?}) expects 1 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let bits = match arg {
        Value::Bool(b) => *b as u64,
        Value::Int(v) => *v as u64,
        other => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("Hash.hash ({impl_:?}) expects a Bool/Int operand; got {other:?}",),
            });
        }
    };
    Ok(Value::Int(splitmix64(bits) as i64))
}

/// SplitMix64 — the same constants the LLVM emitter inlines so eval
/// and native produce byte-identical hashes for the same input.
fn splitmix64(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}
