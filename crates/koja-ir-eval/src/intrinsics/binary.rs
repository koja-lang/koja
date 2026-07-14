//! `Binary.*` and `Bits.*` family.
//!
//! - `Binary.at(self, index: Int) -> Option<Int>`: O(1) byte read.
//!   Out-of-bounds indices return `None`.
//! - `Binary.byte_size(self) -> Int`: `bytes.len()`.
//! - `Binary.slice(self, range: Range) -> Binary`: copies the
//!   inclusive byte range `[start, stop]`. Endpoints clamp to the
//!   binary's bounds.
//! - `Binary.to_bits(self) -> Bits`: zero-cost widening. Reuses
//!   the existing byte vec with `bit_length = bytes.len() * 8`.
//! - `Binary.to_string(self) -> Result<String, String.ConversionError>`:
//!   UTF-8 validate the bytes and materialize the `Result` enum
//!   via the receiver symbol on `function.return_type`.
//! - `Bits.bit_size(self) -> Int`: the stored `bit_length`.
//! - `Bits.byte_at(self, index: Int) -> Option<Int>`: storage byte
//!   read over the `ceil(bit_length / 8)` bytes the value carries.
//! - `Bits.to_binary(self) -> Result<Binary, String>`: require
//!   byte-aligned bit_length and return `Ok(Binary)`, else
//!   `Err(reason)`.

use std::str;

use koja_ir::{BinaryMethod, BitsMethod, IRFunction};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn binary<R: CallResolver>(
    method: BinaryMethod,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match method {
        BinaryMethod::At => at(function, args),
        BinaryMethod::ByteSize => byte_size(args),
        BinaryMethod::Slice => slice(args),
        BinaryMethod::ToBits => to_bits(args),
        BinaryMethod::ToString => to_string(function, args, resolver),
    }
}

pub(super) fn bits(
    method: BitsMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        BitsMethod::BitSize => bit_size(args),
        BitsMethod::ByteAt => byte_at(function, args),
        BitsMethod::ToBinary => bits_to_binary(function, args),
    }
}

fn bit_size(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Bits { bit_length, .. }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bits.bit_size expects a single Bits argument, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::Int(*bit_length as i64))
}

fn byte_at(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Bits { bytes, .. }, Value::Int(index)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bits.byte_at expects (Bits, Int) arguments, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let option_symbol = helpers::enum_return_symbol(function, "Bits.byte_at")?;
    let byte = usize::try_from(*index)
        .ok()
        .and_then(|i| bytes.get(i))
        .map(|b| Value::Int(*b as i64));
    Ok(helpers::option_value(option_symbol, byte))
}

fn at(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes), Value::Int(index)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.at expects (Binary, Int) arguments, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let option_symbol = helpers::enum_return_symbol(function, "Binary.at")?;
    let byte = usize::try_from(*index)
        .ok()
        .and_then(|i| bytes.get(i))
        .map(|b| Value::Int(*b as i64));
    Ok(helpers::option_value(option_symbol, byte))
}

fn slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes), Value::Struct { fields, .. }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.slice expects (Binary, Range) arguments, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let [Value::Int(start), Value::Int(stop)] = fields.as_slice() else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("Binary.slice: Range must hold (Int, Int) fields, got {fields:?}"),
        });
    };
    let len = bytes.len();
    let start = ((*start).max(0) as usize).min(len);
    let stop = ((*stop + 1).max(0) as usize).min(len).max(start);
    Ok(Value::binary(&bytes[start..stop]))
}

fn byte_size(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.byte_size expects a single Binary argument, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::Int(bytes.len() as i64))
}

fn to_bits(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.to_bits expects a single Binary argument, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let bit_length = (bytes.len() as u64) * 8;
    Ok(Value::Bits {
        bytes: bytes.clone(),
        bit_length,
    })
}

fn to_string<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.to_string expects a single Binary argument, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Binary.to_string")?;
    let parsed = match str::from_utf8(bytes) {
        Ok(_) => Ok(Value::String(bytes.clone())),
        Err(_) => Err(helpers::err_variant_value(
            &result_symbol,
            resolver,
            "InvalidUTF8",
        )?),
    };
    Ok(helpers::result_value(result_symbol, parsed))
}

fn bits_to_binary(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Bits { bytes, bit_length }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bits.to_binary expects a single Bits argument, got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Bits.to_binary")?;
    let parsed = if bit_length.is_multiple_of(8) {
        Ok(Value::Binary(bytes.clone()))
    } else {
        Err(Value::string(format!(
            "Bits.to_binary: bit_length {bit_length} is not a multiple of 8 (payload \
             has a trailing partial byte)"
        )))
    };
    Ok(helpers::result_value(result_symbol, parsed))
}
