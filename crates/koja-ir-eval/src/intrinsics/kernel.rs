//! `Kernel.panic(message: String)` — surface the user-supplied
//! message as [`RuntimeError::Panicked`] so test harnesses can match
//! on it. The LLVM backend's parallel emitter calls
//! `__koja_panic`, which prints `** (panic) <message>` plus a
//! backtrace to stderr and aborts; the eval interpreter doesn't tear
//! down the host process, instead it bubbles the message up the same
//! way every other runtime error does.

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn panic(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::String(bytes)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Kernel.panic expects a single String argument; got {} arg(s): {args:?}",
                args.len(),
            ),
        });
    };
    Err(RuntimeError::Panicked {
        message: String::from_utf8_lossy(bytes).into_owned(),
    })
}
