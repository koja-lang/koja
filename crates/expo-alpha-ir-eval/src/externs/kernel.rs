//! `@extern "C" fn expo_kernel_exit(code: Int64)` — terminates the
//! host process. Calls straight into [`expo_runtime`]'s
//! `expo_kernel_exit` over the C ABI so eval observes the same
//! `std::process::exit` the LLVM backend would.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn expo_kernel_exit(code: i64) -> !;
}

pub(super) fn exit(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(code)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "expo_kernel_exit expects a single Int64 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    unsafe { expo_kernel_exit(*code) }
}
