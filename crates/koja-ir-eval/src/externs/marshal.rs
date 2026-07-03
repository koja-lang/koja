//! Marshaling core for extern handlers.
//!
//! Most externs are pure pass-throughs: destructure the [`Value`]
//! args, call the C symbol, wrap the raw result. The
//! [`pass_through_externs!`] macro generates the `unsafe extern "C"`
//! declaration *and* the handler for that shape, so a module lists
//! one line per symbol instead of ten:
//!
//! ```ignore
//! pass_through_externs! {
//!     fd_close => fn koja_fd_close(fd: Int32) -> Int32;
//!     fd_read => fn koja_fd_read(fd: Int32, count: Int64) -> CPtr;
//! }
//! ```
//!
//! Arg / return types are the Koja-shaped ABI tokens `Int32`, `Int64`,
//! `CPtr`, plus `()` for returns. Handlers that do anything beyond
//! pass-through (post-call fixups, out-params, `-> !`) stay
//! hand-written next to the macro invocation and share
//! [`type_mismatch`] for their error shape.

use crate::error::RuntimeError;
use crate::value::Value;

/// Uniform arity / shape error for extern handlers.
pub(super) fn type_mismatch(name: &str, signature: &str, args: &[Value]) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!(
            "{name} expects {signature}, got {} arg(s): {args:?}",
            args.len(),
        ),
    }
}

/// Koja ABI token → Rust C-ABI type, usable in type position.
macro_rules! c_type {
    (()) => { () };
    (CPtr) => { *mut u8 };
    (Int32) => { i32 };
    (Int64) => { i64 };
}

/// Koja ABI token + `&Value` → raw C argument. Expands inside a
/// handler. A shape mismatch early-returns the handler's uniform
/// [`type_mismatch`](super::marshal::type_mismatch) error.
macro_rules! unmarshal_arg {
    (CPtr, $value:expr, $symbol:expr, $signature:expr, $args:expr) => {
        match $value {
            $crate::value::Value::CPtr(v) => *v,
            _ => {
                return Err($crate::externs::marshal::type_mismatch(
                    $symbol, $signature, $args,
                ));
            }
        }
    };
    (Int32, $value:expr, $symbol:expr, $signature:expr, $args:expr) => {
        match $value {
            $crate::value::Value::Int(v) => *v as i32,
            _ => {
                return Err($crate::externs::marshal::type_mismatch(
                    $symbol, $signature, $args,
                ));
            }
        }
    };
    (Int64, $value:expr, $symbol:expr, $signature:expr, $args:expr) => {
        match $value {
            $crate::value::Value::Int(v) => *v,
            _ => {
                return Err($crate::externs::marshal::type_mismatch(
                    $symbol, $signature, $args,
                ));
            }
        }
    };
}

/// Koja ABI token + raw C result → [`Value`].
macro_rules! marshal_return {
    ((), $raw:expr) => {{
        let _: () = $raw;
        $crate::value::Value::Unit
    }};
    (CPtr, $raw:expr) => {
        $crate::value::Value::CPtr($raw)
    };
    (Int32, $raw:expr) => {
        $crate::value::Value::Int(i64::from($raw))
    };
    (Int64, $raw:expr) => {
        $crate::value::Value::Int($raw)
    };
}

/// Generate the extern declaration + pass-through handler for each
/// `handler => fn c_symbol(args…) -> ret;` entry. See the module
/// docs for the shape.
macro_rules! pass_through_externs {
    ($($handler:ident => fn $symbol:ident($($arg:ident: $aty:tt),* $(,)?) -> $rty:tt;)*) => {
        unsafe extern "C" {
            $(
                fn $symbol(
                    $($arg: $crate::externs::marshal::c_type!($aty)),*
                ) -> $crate::externs::marshal::c_type!($rty);
            )*
        }

        $(
            pub(super) fn $handler(
                args: &[$crate::value::Value],
            ) -> Result<$crate::value::Value, $crate::error::RuntimeError> {
                const SYMBOL: &str = stringify!($symbol);
                const SIGNATURE: &str = concat!("(", stringify!($($arg: $aty),*), ")");
                let [$($arg),*] = args else {
                    return Err($crate::externs::marshal::type_mismatch(SYMBOL, SIGNATURE, args));
                };
                $(
                    let $arg = $crate::externs::marshal::unmarshal_arg!(
                        $aty, $arg, SYMBOL, SIGNATURE, args
                    );
                )*
                let raw = unsafe { $symbol($($arg),*) };
                Ok($crate::externs::marshal::marshal_return!($rty, raw))
            }
        )*
    };
}

pub(super) use {c_type, marshal_return, pass_through_externs, unmarshal_arg};
