//! Externs declared in `lib/global/src/kernel.koja`.
//!
//! - `@extern "C" fn koja_kernel_exit(code: Int64)` — terminates the
//!   host process. Calls straight into [`koja_runtime`]'s
//!   `koja_kernel_exit` over the C ABI so eval observes the same
//!   `std::process::exit` the LLVM backend would.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn koja_kernel_exit(code: i64) -> !;
}

pub(super) fn exit(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(code)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "koja_kernel_exit expects a single Int64 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    unsafe { koja_kernel_exit(*code) }
}
