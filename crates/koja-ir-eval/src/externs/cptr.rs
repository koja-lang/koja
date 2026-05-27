//! Externs declared in `lib/global/src/cptr.koja`.
//!
//! - `@extern "C" fn strlen(s: CPtr<UInt8>) -> Int64` — libc's
//!   `strlen`. Used by `CPtr<UInt8>.to_cstring` (private) to compute
//!   the length of a null-terminated C string. Calls straight into
//!   libc so eval observes the same byte count the LLVM backend
//!   would.

use crate::error::RuntimeError;
use crate::value::Value;

unsafe extern "C" {
    fn strlen(s: *const u8) -> usize;
}

pub(super) fn strlen_(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "strlen expects a single CPtr<UInt8> argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "strlen(null) is undefined behavior; refusing to call libc".to_string(),
        });
    }
    let len = unsafe { strlen(*ptr as *const u8) };
    Ok(Value::Int(len as i64))
}
