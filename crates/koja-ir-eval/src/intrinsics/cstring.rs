//! Checked `CString.to_string` conversion.

use std::slice;
use std::str;

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::{IntrinsicCall, helpers};
use crate::value::Value;

pub(super) fn to_string<R: CallResolver>(
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    let [Value::Struct { fields, .. }] = call.args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "CString.to_string expects a single CString struct, got {:?}",
                call.args
            ),
        });
    };
    let [Value::CPtr(ptr), Value::Int(len)] = fields.as_slice() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "CString.to_string: receiver fields must be `(CPtr<UInt8>, Int)`, got {fields:?}",
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(call.function, "CString.to_string")?;
    let converted = if *len < 0 {
        Err(helpers::err_variant_value(
            &result_symbol,
            call.resolver,
            "InvalidLength",
        )?)
    } else if *len > 0 && ptr.is_null() {
        Err(helpers::err_variant_value(
            &result_symbol,
            call.resolver,
            "NullPointer",
        )?)
    } else {
        let bytes = if *len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(*ptr as *const u8, *len as usize) }.to_vec()
        };
        match str::from_utf8(&bytes) {
            Ok(_) => Ok(Value::string(bytes)),
            Err(_) => Err(helpers::err_variant_value(
                &result_symbol,
                call.resolver,
                "InvalidUTF8",
            )?),
        }
    };
    helpers::result_value(result_symbol, call.resolver, converted)
}
