//! Externs declared in `lib/global/src/cptr.koja`.

use crate::error::RuntimeError;
use crate::externs::marshal::type_mismatch;
use crate::value::Value;

unsafe extern "C" {
    fn strlen(s: *const u8) -> usize;
}

pub(super) fn strlen_(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(type_mismatch("strlen", "(s: CPtr<UInt8>)", args));
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "strlen(null) is undefined behavior, refusing to call libc".to_string(),
        });
    }
    let len = unsafe { strlen(*ptr as *const u8) };
    Ok(Value::Int(len as i64))
}
