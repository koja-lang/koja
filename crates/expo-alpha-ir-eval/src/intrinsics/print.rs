//! `@intrinsic Global.print(s: String) -> Unit` — write the string
//! to stdout with a trailing newline. Matches the LLVM-side
//! [`__expo_alpha_print_string`] runtime contract byte-for-byte so
//! both backends produce identical stdout for `print(...)` calls.
//!
//! [`__expo_alpha_print_string`]: ../../../expo-runtime/src/alpha.rs

use std::io::{self, Write};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn global_print(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::String(payload)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Global.print expects a single String argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{payload}").map_err(|e| RuntimeError::Unsupported {
        detail: format!("stdout write failed: {e}"),
    })?;
    Ok(Value::Unit)
}
