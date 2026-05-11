//! Externs declared in `lib/global/src/random.expo`.
//!
//! - `@extern "C" fn expo_random_bytes(count: Int64) -> CPtr<UInt8>`
//!   and `@extern "C" fn expo_random_int(min: Int64, max: Int64) -> Int64`
//!   — the runtime entropy primitives `Random.bytes` / `Random.int`
//!   delegate to. Both call straight into [`expo_runtime`] over the
//!   C ABI so eval consumes the same OS entropy the LLVM backend
//!   would.
//!
//! `bytes` returns a length-prefixed Expo-string payload pointer
//! (the runtime allocates `[i64 bit_length][payload…]` with `malloc`
//! and returns the payload offset). Eval wraps it as
//! [`Value::CPtr`]; consumers walk the standard `CPtr<UInt8>` chain
//! (`.to_string().to_binary()` for the `Random.bytes` body).

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn expo_random_bytes(count: i64) -> *mut u8;
    fn expo_random_int(min: i64, max: i64) -> i64;
}

pub(super) fn bytes(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(count)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "expo_random_bytes expects a single Int64 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let ptr = unsafe { expo_random_bytes(*count) };
    Ok(Value::CPtr(ptr))
}

pub(super) fn int(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(min), Value::Int(max)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "expo_random_int expects two Int64 arguments; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let value = unsafe { expo_random_int(*min, *max) };
    Ok(Value::Int(value))
}
