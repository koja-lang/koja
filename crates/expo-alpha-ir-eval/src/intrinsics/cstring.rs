//! `CString.to_string(self) -> String` — copies the bytes pointed
//! at by the CString's `ptr: CPtr<UInt8>` (with byte length carried
//! by the sibling `len: Int` field) into a fresh Expo `String`.
//! The CString's underlying buffer is left untouched — callers
//! free it explicitly via `CString.free`.

use std::slice;

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn to_string(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Struct { fields, .. }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CString.to_string expects a single CString struct; got {args:?}"),
        });
    };
    let [Value::CPtr(ptr), Value::Int(len)] = fields.as_slice() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "CString.to_string: receiver fields must be `(CPtr<UInt8>, Int)`; got {fields:?}",
            ),
        });
    };
    let len = (*len).max(0) as usize;
    if len == 0 {
        return Ok(Value::String(Vec::new()));
    }
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "CString.to_string(ptr=null, len>0) is undefined behavior; refusing to copy"
                .to_string(),
        });
    }
    let bytes = unsafe { slice::from_raw_parts(*ptr as *const u8, len) }.to_vec();
    Ok(Value::String(bytes))
}
