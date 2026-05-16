//! `Binary.*` and `Bits.to_binary` family.
//!
//! - `Binary.byte_size(self) -> Int` — `bytes.len()`.
//! - `Binary.ptr(self) -> CPtr<UInt8>` — copies the byte payload
//!   into a fresh length-prefixed Expo-string buffer (the v1
//!   `[i64 bit_length][payload…]` ABI) so the caller can hand it
//!   to C code. The buffer is `malloc`-allocated; the caller owns
//!   it and must `free` (via `CPtr.free` or the runtime's
//!   `expo_free` shim) when done. Mirrors the LLVM backend's
//!   shape: `Binary` is itself heap-backed there, so `.ptr()` just
//!   hands out the existing payload offset — eval has to copy
//!   because `Value::Binary` owns a `Vec<u8>` with no stable
//!   address guarantee, but the *observable* C-side shape is
//!   identical.
//! - `Binary.to_bits(self) -> Bits` — zero-cost widening; reuses
//!   the existing byte vec with `bit_length = bytes.len() * 8`.
//! - `Binary.to_string(self) -> Result<String, String>` —
//!   UTF-8 validate the bytes and materialize the `Result` enum
//!   via the receiver symbol on `function.return_type`.
//! - `Bits.to_binary(self) -> Result<Binary, String>` — require
//!   byte-aligned bit_length and return `Ok(Binary)`; else
//!   `Err(reason)`.

use std::ptr;

use expo_ir::{BinaryMethod, BitsMethod, IRFunction};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::Value;

const STRING_HEADER_SIZE: usize = 8;
const BITS_PER_BYTE: i64 = 8;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
}

pub(super) fn binary(
    method: BinaryMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        BinaryMethod::ByteSize => byte_size(args),
        BinaryMethod::Clone => clone(args),
        BinaryMethod::Ptr => ptr_(args),
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
        BitsMethod::Clone => bits_clone(args),
        BitsMethod::ToBinary => bits_to_binary(function, args),
    }
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

fn clone(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Binary(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Binary.clone expects a single Binary argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::Binary(bytes.clone()))
}

fn bits_clone(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::Bits { bytes, bit_length }] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Bits.clone expects a single Bits argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Ok(Value::Bits {
        bytes: bytes.clone(),
        bit_length: *bit_length,
    })
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
    Ok(Value::CPtr(alloc_expo_string_payload(bytes)))
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

/// Copy `data` into a freshly-`malloc`'d `[i64 bit_length][payload…]`
/// buffer and return a pointer to the payload (matches
/// [`expo_runtime::util::alloc_binary`]'s ABI). Empty inputs round-
/// trip as a null pointer because the runtime helpers do the same:
/// a zero-byte payload has no meaningful address. Callers that need
/// to free pass the *payload* pointer back through `CPtr.free` or
/// the runtime's `expo_free`, which both step back over the header.
fn alloc_expo_string_payload(data: &[u8]) -> *mut u8 {
    if data.is_empty() {
        return ptr::null_mut();
    }
    let total = STRING_HEADER_SIZE + data.len();
    let base = unsafe { malloc(total) };
    if base.is_null() {
        return ptr::null_mut();
    }
    let bit_len = (data.len() as i64) * BITS_PER_BYTE;
    unsafe {
        *(base as *mut i64) = bit_len;
        let payload = base.add(STRING_HEADER_SIZE);
        ptr::copy_nonoverlapping(data.as_ptr(), payload, data.len());
        payload
    }
}
