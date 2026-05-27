//! `@intrinsic Global.print(s: String) -> Unit` — write the string
//! to stdout with a trailing newline. Matches the LLVM-side
//! [`__koja_print_string`] runtime contract byte-for-byte so
//! both backends produce identical stdout for `print(...)` calls.
//!
//! [`__koja_print_string`]: ../../../koja-runtime/src/intrinsics.rs

use std::io::{self, Write};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn global_print(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::String(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Global.print expects a single String argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|_| stdout.write_all(b"\n"))
        .map_err(|e| RuntimeError::Unsupported {
            detail: format!("stdout write failed: {e}"),
        })?;
    Ok(Value::Unit)
}
