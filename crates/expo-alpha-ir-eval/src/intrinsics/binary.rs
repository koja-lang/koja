//! `Binary.*` and `Bits.to_binary` family.
//!
//! - `Binary.byte_size(self) -> Int` — `bytes.len()`.
//! - `Binary.ptr(self) -> CPtr<UInt8>` — depends on a `CPtr` value
//!   variant the eval interpreter doesn't carry; surfaces
//!   [`RuntimeError::Unsupported`].
//! - `Binary.to_bits(self) -> Bits` — zero-cost widening; reuses the
//!   existing byte vec with `bit_length = bytes.len() * 8`.
//! - `Binary.to_string(self) -> Result<String, String>` and
//!   `Bits.to_binary(self) -> Result<Binary, String>` — both return
//!   `Result<_, _>`, which can't be materialized without a registry
//!   handle to the enum decl. Mirrors the LLVM-side stub
//!   ([`crate::intrinsics::binary`]) by surfacing
//!   [`RuntimeError::Unsupported`].

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn matches_id(id: &str) -> bool {
    matches!(
        id,
        "Binary.byte_size"
            | "Binary.ptr"
            | "Binary.to_bits"
            | "Binary.to_string"
            | "Bits.to_binary"
    )
}

pub(super) fn dispatch(id: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    match id {
        "Binary.byte_size" => byte_size(args),
        "Binary.to_bits" => to_bits(args),
        "Binary.ptr" | "Binary.to_string" | "Bits.to_binary" => Err(RuntimeError::Unsupported {
            detail: format!(
                "`{id}` is not implemented in the eval interpreter — \
                 mirrors the LLVM-side stub. Use `--backend=llvm`.",
            ),
        }),
        other => panic!("intrinsics::binary: unhandled id `{other}`"),
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
