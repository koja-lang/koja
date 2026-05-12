//! Eval handlers for the `Debug.format` intrinsic family — the 8
//! integer cells, `Float` / `Float32`, and `Bool`. Mirrors
//! `expo_format_*` in `expo-runtime/src/format.rs` byte-for-byte
//! so eval output stays exactly aligned with the LLVM backend's
//! native rendering.
//!
//! `String.format` ships a pure-Expo body in
//! `lib/global/src/debug.expo` and never reaches the intrinsic
//! dispatch; receiver shapes outside [`DebugImpl`] surface a typed
//! [`RuntimeError::TypeMismatch`].

use expo_alpha_ir::{DebugImpl, IntType};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(impl_: DebugImpl, args: &[Value]) -> Result<Value, RuntimeError> {
    let [value] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Debug.format ({impl_:?}) expects 1 argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let rendered = match (impl_, value) {
        (DebugImpl::Bool, Value::Bool(b)) => format!("{b}"),
        (DebugImpl::Float, Value::Float64(v)) => format!("{v:?}"),
        (DebugImpl::Float32, Value::Float32(v)) => format!("{v:?}"),
        (DebugImpl::Int(ty), Value::Int(v)) => format_int(ty, *v),
        _ => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("Debug.format ({impl_:?}) received incompatible value `{value:?}`",),
            });
        }
    };
    Ok(Value::String(rendered.into_bytes()))
}

/// Signed widths render via `{}` (decimal, optional `-`); unsigned
/// widths reinterpret the stored `i64` through `u64` so values with
/// the high bit set render as the original unsigned magnitude
/// (matching the LLVM backend's `expo_format_u64` path).
fn format_int(ty: IntType, raw: i64) -> String {
    if ty.is_signed() {
        format!("{raw}")
    } else {
        format!("{}", raw as u64)
    }
}
