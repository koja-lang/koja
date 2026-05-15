//! Externs declared in `lib/global/src/time.expo`.
//!
//! - `@extern "C" fn expo_time_now_millis() -> Int64` — current
//!   wall-clock time in milliseconds since the Unix epoch. Calls
//!   straight into [`expo_runtime`]'s `expo_time_now_millis` over
//!   the C ABI so eval observes the same instant the LLVM backend
//!   would.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn expo_time_now_millis() -> i64;
}

pub(super) fn now_millis(args: &[Value]) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "expo_time_now_millis expects 0 arguments; got {}",
                args.len(),
            ),
        });
    }
    let millis = unsafe { expo_time_now_millis() };
    Ok(Value::Int(millis))
}
