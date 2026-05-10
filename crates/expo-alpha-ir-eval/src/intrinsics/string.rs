//! `String` method intrinsics. Eval-side codepoint walking goes
//! through Rust's `str` primitives so semantics match v1 byte-for-
//! byte. `to_cstring` errors with [`RuntimeError::Unsupported`] —
//! eval has no `CPtr` value variant (mirrors
//! [`crate::intrinsics::cstring::to_string`]), so the conversion is
//! LLVM-only.

use expo_alpha_ir::{IRFunction, IRSymbol, IRType, IRVariantTag, StringMethod};

use crate::error::RuntimeError;
use crate::value::{EnumPayload, Value};

const SOME_TAG: IRVariantTag = IRVariantTag(0);
const NONE_TAG: IRVariantTag = IRVariantTag(1);

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
        StringMethod::ToCstring => to_cstring(),
    }
}

fn byte_length(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string(args, 0, "String.byte_length")?;
    Ok(Value::Int(s.len() as i64))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string(args, 0, "String.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

fn to_binary(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string(args, 0, "String.to_binary")?;
    Ok(Value::String(s.to_string()))
}

fn to_cstring() -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: "`String.to_cstring` is not implemented in the eval interpreter — \
             CString carries a CPtr<UInt8> with no in-process \
             representation. Use `--backend=llvm`."
            .to_string(),
    })
}

fn get(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string(args, 0, "String.get")?;
    let index = expect_int(args, 1, "String.get")?;
    let option_symbol = enum_return_symbol(function, "String.get")?;
    let value = if index < 0 {
        None
    } else {
        s.chars()
            .nth(index as usize)
            .map(|c| Value::String(c.to_string()))
    };
    Ok(option_value(option_symbol, value))
}

fn slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_string(args, 0, "String.slice")?;
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
    Ok(Value::String(s[byte_start..byte_end].to_string()))
}

fn expect_arg<'a>(args: &'a [Value], index: usize, label: &str) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::Unsupported {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

fn expect_string<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::String(s) => Ok(s.as_str()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected String, got `{other}`"),
        }),
    }
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

fn enum_return_symbol(function: &IRFunction, label: &str) -> Result<IRSymbol, RuntimeError> {
    match &function.return_type {
        IRType::Enum(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected Enum return type, got `{other:?}`"),
        }),
    }
}

fn option_value(symbol: IRSymbol, value: Option<Value>) -> Value {
    match value {
        Some(v) => Value::Enum {
            name: "Some".into(),
            payload: EnumPayload::Tuple(vec![v]),
            symbol,
            tag: SOME_TAG,
        },
        None => Value::Enum {
            name: "None".into(),
            payload: EnumPayload::Unit,
            symbol,
            tag: NONE_TAG,
        },
    }
}
