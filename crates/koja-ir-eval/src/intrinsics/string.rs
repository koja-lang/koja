//! `String` method intrinsics. Eval-side codepoint walking goes
//! through Rust's `str` primitives so semantics match v1 byte-for-
//! byte. `to_cstring` allocates a null-terminated `malloc` copy of
//! the receiver and bundles the pointer + byte length into a
//! [`Value::Struct`] matching the `CString` decl — callers free it
//! through `CString.free` (which routes to `CPtr.free`).

use std::ptr;

use koja_ir::{IRFunction, IRSymbol, IRType, StringMethod};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
}

pub(super) fn dispatch(
    method: StringMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        StringMethod::ByteLength => byte_length(args),
        StringMethod::Get => get(function, args),
        StringMethod::Length => length(args),
        StringMethod::Slice => slice(args),
        StringMethod::ToBinary => to_binary(args),
        StringMethod::ToCstring => to_cstring(function, args),
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
    Ok(Value::Binary(bytes.to_vec()))
}

fn to_cstring(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let bytes = expect_string_bytes(args, 0, "String.to_cstring")?;
    let cstring_symbol = struct_return_symbol(function, "String.to_cstring")?;
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
    Ok(Value::Struct {
        symbol: cstring_symbol,
        fields: vec![Value::CPtr(buf), Value::Int(bytes.len() as i64)],
    })
}

fn get(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(args, 0, "String.get")?;
    let index = expect_int(args, 1, "String.get")?;
    let option_symbol = helpers::enum_return_symbol(function, "String.get")?;
    let value = if index < 0 {
        None
    } else {
        s.chars()
            .nth(index as usize)
            .map(|c| Value::String(c.to_string().into_bytes()))
    };
    Ok(helpers::option_value(option_symbol, value))
}

fn slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string_utf8(args, 0, "String.slice")?;
    let range = expect_range(args, 1, "String.slice")?;
    let len = s.chars().count();
    let start = (range.0.max(0) as usize).min(len);
    let stop = ((range.1 + 1).max(0) as usize).min(len).max(start);
    let byte_start = s
        .char_indices()
        .nth(start)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let byte_end = if stop == len {
        s.len()
    } else {
        s.char_indices()
            .nth(stop)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    };
    Ok(Value::String(s.as_bytes()[byte_start..byte_end].to_vec()))
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
/// UTF-8 — codepoint-walking methods (`length`, `get`, `slice`)
/// can't behave sensibly without it. Byte-oriented methods
/// (`byte_length`, `to_binary`, `to_cstring`) read raw bytes via
/// [`expect_string_bytes`] instead.
fn expect_string_utf8<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, RuntimeError> {
    let bytes = expect_string_bytes(args, index, label)?;
    std::str::from_utf8(bytes).map_err(|err| RuntimeError::Unsupported {
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
/// struct value — typecheck guarantees the Range shape (two `Int`
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

fn struct_return_symbol(function: &IRFunction, label: &str) -> Result<IRSymbol, RuntimeError> {
    match &function.return_type {
        IRType::Struct(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected Struct return type, got `{other:?}`"),
        }),
    }
}
