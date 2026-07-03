//! Eval handlers for the `Debug.format` intrinsic family: the 8
//! integer cells, `Float` / `Float32`, and `Bool`. Mirrors
//! `koja_format_*` in `koja-runtime-posix/src/format.rs` byte-for-byte
//! so eval output stays exactly aligned with the LLVM backend's
//! native rendering.
//!
//! `String.format` ships a pure-Koja body in
//! `lib/global/src/debug.koja` and never reaches the intrinsic
//! dispatch. Receiver shapes outside [`DebugImpl`] surface a typed
//! [`RuntimeError::TypeMismatch`].

use koja_ir::{DebugImpl, IntType};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(impl_: DebugImpl, args: &[Value]) -> Result<Value, RuntimeError> {
    let [value] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Debug.format ({impl_:?}) expects 1 argument, got {} arg(s): {args:?}",
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
    Ok(Value::string(rendered))
}

/// Signed widths render via `{}` (decimal, optional `-`). Unsigned
/// widths reinterpret the stored `i64` through `u64` so values with
/// the high bit set render as the original unsigned magnitude
/// (matching the LLVM backend's `koja_format_u64` path).
fn format_int(ty: IntType, raw: i64) -> String {
    if ty.is_signed() {
        format!("{raw}")
    } else {
        format!("{}", raw as u64)
    }
}
