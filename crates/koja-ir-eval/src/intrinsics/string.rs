//! `String` method intrinsics. Eval-side codepoint walking goes
//! through Rust's `str` primitives so semantics match the native
//! backend byte-for-byte.
//! `to_cstring` allocates a null-terminated `malloc` copy of
//! the receiver and bundles the pointer + byte length into a
//! [`Value::Struct`] matching the `CString` decl. Callers free it
//! through `CString.free` (which routes to `CPtr.free`).

use std::{ptr, str};

use koja_ir::{IRSymbol, IRType, StringMethod};
use koja_runtime::{codepoint_range_to_bytes, find_bytes, is_utf8_boundary};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::{IntrinsicCall, helpers};
use crate::value::Value;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
}

pub(super) fn dispatch<R: CallResolver>(
    method: StringMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        StringMethod::ByteLength => byte_length(call.args),
        StringMethod::Find => find(call),
        StringMethod::Get => get(call),
        StringMethod::Length => length(call.args),
        StringMethod::Next => next(call),
        StringMethod::Slice => slice(call.args),
        StringMethod::SliceBytes => slice_bytes(call.args),
        StringMethod::ToBinary => to_binary(call.args),
        StringMethod::ToCstring => to_cstring(call),
    }
}

fn byte_length(args: &[Value]) -> Result<Value, RuntimeError> {
    let bytes = expect_string_bytes(args, 0, "String.byte_length")?;
    Ok(Value::Int(bytes.len() as i64))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(args, 0, "String.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

fn to_binary(args: &[Value]) -> Result<Value, RuntimeError> {
    let bytes = expect_string_bytes(args, 0, "String.to_binary")?;
    Ok(Value::binary(bytes))
}

fn to_cstring<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let bytes = expect_string_bytes(call.args, 0, "String.to_cstring")?;
    let result_symbol = helpers::enum_return_symbol(call.function, "String.to_cstring")?;
    if bytes.contains(&0) {
        let error = helpers::err_variant_value(&result_symbol, call.resolver, "InteriorNul")?;
        return helpers::result_value(result_symbol, call.resolver, Err(error));
    }
    let cstring_symbol = result_struct_symbol(
        &helpers::single_ok_payload(&result_symbol, call.resolver, "String.to_cstring")?,
        "String.to_cstring",
    )?;
    let total = bytes.len() + 1; // null terminator
    let buf = unsafe { malloc(total) };
    if buf.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "String.to_cstring: malloc returned null".to_string(),
        });
    }
    unsafe {
        if !bytes.is_empty() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        }
        *buf.add(bytes.len()) = 0;
    }
    let cstring = Value::Struct {
        symbol: cstring_symbol,
        fields: vec![Value::CPtr(buf), Value::Int(bytes.len() as i64)],
    };
    helpers::result_value(result_symbol, call.resolver, Ok(cstring))
}

fn get<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(call.args, 0, "String.get")?;
    let index = expect_int(call.args, 1, "String.get")?;
    let option_symbol = helpers::enum_return_symbol(call.function, "String.get")?;
    let value = if index < 0 {
        None
    } else {
        s.chars()
            .nth(index as usize)
            .map(|c| Value::string(c.to_string()))
    };
    helpers::option_value(option_symbol, call.resolver, value)
}

fn next<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(call.args, 0, "String.next")?;
    let byte_offset = expect_int(call.args, 1, "String.next")?;
    let option_symbol = helpers::enum_return_symbol(call.function, "String.next")?;
    let value = usize::try_from(byte_offset)
        .ok()
        .and_then(|offset| s.get(offset..).map(|suffix| (offset, suffix)))
        .and_then(|(offset, suffix)| {
            suffix.chars().next().map(|character| {
                Value::Tuple(vec![
                    Value::string(character.to_string()),
                    Value::Int((offset + character.len_utf8()) as i64),
                ])
            })
        });
    helpers::option_value(option_symbol, call.resolver, value)
}

fn slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(args, 0, "String.slice")?;
    let range = expect_range(args, 1, "String.slice")?;
    let (byte_start, byte_end) = codepoint_range_to_bytes(s, range.0, range.1);
    Ok(Value::string(&s.as_bytes()[byte_start..byte_end]))
}

fn find<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let haystack = expect_string_bytes(call.args, 0, "String.find")?;
    let needle = expect_string_bytes(call.args, 1, "String.find")?;
    let from = expect_int(call.args, 2, "String.find")?;
    let option_symbol = helpers::enum_return_symbol(call.function, "String.find")?;
    let offset = find_bytes(haystack, needle, from).map(|offset| Value::Int(offset as i64));
    helpers::option_value(option_symbol, call.resolver, offset)
}

/// Byte-range copy over `[start, stop)`. Endpoints clamp to the byte
/// length and must land on codepoint boundaries. Works on raw bytes
/// with an O(1) boundary check because a full UTF-8 validation per
/// call would make split-style loops quadratic again.
fn slice_bytes(args: &[Value]) -> Result<Value, RuntimeError> {
    let bytes = expect_string_bytes(args, 0, "String.slice_bytes")?;
    let start = expect_int(args, 1, "String.slice_bytes")?;
    let stop = expect_int(args, 2, "String.slice_bytes")?;
    let start = (start.max(0) as usize).min(bytes.len());
    let stop = (stop.max(0) as usize).min(bytes.len()).max(start);
    if !is_utf8_boundary(bytes, start) || !is_utf8_boundary(bytes, stop) {
        return Err(RuntimeError::Unsupported {
            detail: "String.slice_bytes offsets must land on codepoint boundaries".to_string(),
        });
    }
    Ok(Value::string(&bytes[start..stop]))
}

fn expect_arg<'a>(args: &'a [Value], index: usize, label: &str) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::Unsupported {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

fn expect_string_bytes<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a [u8], RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::String(bytes) => Ok(bytes.as_slice()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected String, got `{other}`"),
        }),
    }
}

/// Borrow a String arg as `&str`. Surfaces a clean
/// [`RuntimeError::Unsupported`] when the payload isn't valid
/// UTF-8: codepoint-walking methods (`length`, `get`, `slice`)
/// can't behave sensibly without it. Byte-oriented methods
/// (`byte_length`, `to_binary`, `to_cstring`) read raw bytes via
/// [`expect_string_bytes`] instead.
fn expect_string_utf8<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, RuntimeError> {
    let bytes = expect_string_bytes(args, index, label)?;
    str::from_utf8(bytes).map_err(|err| RuntimeError::Unsupported {
        detail: format!(
            "{label} arg #{index}: String contents are not valid UTF-8 \
             (invalid at byte {}): {err}",
            err.valid_up_to(),
        ),
    })
}

fn expect_int(args: &[Value], index: usize, label: &str) -> Result<i64, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::Int(value) => Ok(*value),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected Int, got `{other}`"),
        }),
    }
}

/// Extract `(start, stop)` from a `Range { start: Int, stop: Int }`
/// struct value. Typecheck guarantees the Range shape (two `Int`
/// fields in source order) before we reach here.
fn expect_range(args: &[Value], index: usize, label: &str) -> Result<(i64, i64), RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::Struct { fields, .. } if fields.len() == 2 => {
            let start = match &fields[0] {
                Value::Int(v) => *v,
                other => {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("{label}: Range.start expected Int, got `{other}`"),
                    });
                }
            };
            let stop = match &fields[1] {
                Value::Int(v) => *v,
                other => {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("{label}: Range.stop expected Int, got `{other}`"),
                    });
                }
            };
            Ok((start, stop))
        }
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected Range struct, got `{other}`"),
        }),
    }
}

fn result_struct_symbol(ty: &IRType, label: &str) -> Result<IRSymbol, RuntimeError> {
    match ty {
        IRType::Struct(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected a Struct Ok payload, got `{other:?}`"),
        }),
    }
}
