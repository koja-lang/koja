//! Eval handlers for the `Hash` intrinsic family — `Bool` and the
//! 8 integer cells (flattened to [`Value::Int(i64)`]) feed their
//! native bit pattern through SplitMix64. `String` walks each byte
//! of the UTF-8 payload through FNV-1a (offset basis
//! `0xcbf29ce484222325`, prime `0x100000001b3`) so eval and native
//! produce byte-identical hash codes for the same input — see the
//! LLVM-side [`crate::intrinsics::hash::emit_string_hash`] for the
//! IR-level twin.

use expo_alpha_ir::HashImpl;

use crate::error::RuntimeError;
use crate::value::Value;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

pub(super) fn dispatch(impl_: HashImpl, args: &[Value]) -> Result<Value, RuntimeError> {
    let [arg] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Hash.hash ({impl_:?}) expects 1 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let mixed = match (impl_, arg) {
        (HashImpl::Bool, Value::Bool(b)) => splitmix64(*b as u64),
        (HashImpl::Int(_), Value::Int(v)) => splitmix64(*v as u64),
        (HashImpl::String, Value::String(s)) => fnv1a(s.as_bytes()),
        (_, other) => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!(
                    "Hash.hash ({impl_:?}) expects an operand matching the impl cell; \
                     got {other:?}",
                ),
            });
        }
    };
    Ok(Value::Int(mixed as i64))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// SplitMix64 — the same constants the LLVM emitter inlines so eval
/// and native produce byte-identical hashes for the same input.
fn splitmix64(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}
