//! Externs declared in `lib/global/src/time.koja`.
//!
//! - `@extern "C" fn koja_time_now_millis() -> Int64` — current
//!   wall-clock time in milliseconds since the Unix epoch. Calls
//!   straight into [`koja_runtime`]'s `koja_time_now_millis` over
//!   the C ABI so eval observes the same instant the LLVM backend
//!   would.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn koja_time_now_millis() -> i64;
}

pub(super) fn now_millis(args: &[Value]) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "koja_time_now_millis expects 0 arguments; got {}",
                args.len(),
            ),
        });
    }
    let millis = unsafe { koja_time_now_millis() };
    Ok(Value::Int(millis))
}
