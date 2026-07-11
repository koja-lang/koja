//! Checked `CString.to_string` conversion.

use std::slice;
use std::str;

use koja_ir::IRFunction;

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn to_string<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let [Value::Struct { fields, .. }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("CString.to_string expects a single CString struct, got {args:?}"),
        });
    };
    let [Value::CPtr(ptr), Value::Int(len)] = fields.as_slice() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "CString.to_string: receiver fields must be `(CPtr<UInt8>, Int)`, got {fields:?}",
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "CString.to_string")?;
    let converted = if *len < 0 {
        Err(helpers::err_variant_value(
            &result_symbol,
            resolver,
            "InvalidLength",
        )?)
    } else if *len > 0 && ptr.is_null() {
        Err(helpers::err_variant_value(
            &result_symbol,
            resolver,
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
                resolver,
                "InvalidUTF8",
            )?),
        }
    };
    Ok(helpers::result_value(result_symbol, converted))
}
