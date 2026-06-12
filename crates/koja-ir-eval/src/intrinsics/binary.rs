//! `Binary.*` and `Bits.to_binary` family.
//!
//! - `Binary.at(self, index: Int) -> Option<Int>` — O(1) byte read;
//!   out-of-bounds indices return `None`.
//! - `Binary.byte_size(self) -> Int` — `bytes.len()`.
//! - `Binary.ptr(self) -> CPtr<UInt8>` — copies the byte payload
//!   into a fresh rc-prefixed block (see [`crate::abi`]) so the
//!   caller can hand it to C code; the caller owns the block.
//!   Mirrors the LLVM backend's shape: `Binary` is itself
//!   heap-backed there, so `.ptr()` just hands out the existing
//!   payload offset — eval has to copy because `Value::Binary` owns
//!   a `Vec<u8>` with no stable address guarantee, but the
//!   *observable* C-side shape is identical.
//! - `Binary.slice(self, range: Range) -> Binary` — copies the
//!   inclusive byte range `[start, stop]`; endpoints clamp to the
//!   binary's bounds.
//! - `Binary.to_bits(self) -> Bits` — zero-cost widening; reuses
//!   the existing byte vec with `bit_length = bytes.len() * 8`.
//! - `Binary.to_string(self) -> Result<String, String>` —
//!   UTF-8 validate the bytes and materialize the `Result` enum
//!   via the receiver symbol on `function.return_type`.
//! - `Bits.to_binary(self) -> Result<Binary, String>` — require
//!   byte-aligned bit_length and return `Ok(Binary)`; else
//!   `Err(reason)`.

use koja_ir::{BinaryMethod, BitsMethod, IRFunction};

use crate::abi;
use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn binary(
    method: BinaryMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        BinaryMethod::At => at(function, args),
        BinaryMethod::ByteSize => byte_size(args),
        BinaryMethod::Ptr => ptr_(args),
        BinaryMethod::Slice => slice(args),
        BinaryMethod::ToBits => to_bits(args),
        BinaryMethod::ToString => to_string(function, args),
    }
}

pub(super) fn bits(
    method: BitsMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        BitsMethod::ToBinary => bits_to_binary(function, args),
    }
}

fn at(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes), Value::Int(index)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.at expects (Binary, Int) arguments; got {} arg(s): {args:?}",
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
                "Binary.slice expects (Binary, Range) arguments; got {} arg(s): {args:?}",
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
    Ok(Value::Binary(bytes[start..stop].to_vec()))
}

fn byte_size(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.byte_size expects a single Binary argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::Int(bytes.len() as i64))
}

fn ptr_(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.ptr expects a single Binary argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::CPtr(abi::alloc_block(bytes)))
}

fn to_bits(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.to_bits expects a single Binary argument; got {} arg(s): {args:?}",
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

fn to_string(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.to_string expects a single Binary argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Binary.to_string")?;
    let parsed = match std::str::from_utf8(bytes) {
        Ok(_) => Ok(Value::String(bytes.clone())),
        Err(err) => Err(Value::String(
            format!(
                "Binary.to_string: payload is not valid UTF-8 (invalid at byte {}): {err}",
                err.valid_up_to(),
            )
            .into_bytes(),
        )),
    };
    Ok(helpers::result_value(result_symbol, parsed))
}

fn bits_to_binary(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Bits { bytes, bit_length }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bits.to_binary expects a single Bits argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Bits.to_binary")?;
    let parsed = if bit_length.is_multiple_of(8) {
        Ok(Value::Binary(bytes.clone()))
    } else {
        Err(Value::String(
            format!(
                "Bits.to_binary: bit_length {bit_length} is not a multiple of 8 — payload \
                 has a trailing partial byte"
            )
            .into_bytes(),
        ))
    };
    Ok(helpers::result_value(result_symbol, parsed))
}
