//! Externs declared in `lib/global/src/kernel.koja`.

use crate::error::RuntimeError;
use crate::externs::marshal::type_mismatch;
use crate::value::Value;

unsafe extern "C" {
    fn koja_kernel_exit(code: i64) -> !;
}

pub(super) fn exit(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Int(code)] = args else {
        return Err(type_mismatch("koja_kernel_exit", "(code: Int64)", args));
    };
    unsafe { koja_kernel_exit(*code) }
}
