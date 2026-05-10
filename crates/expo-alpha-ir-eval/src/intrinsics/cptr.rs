//! `CPtr<T>` family — `alloc`, `null`, `free`, `offset`, `read`,
//! `write`, `null?`, `to_binary`, `to_string`. The LLVM backend
//! mints a real raw pointer; the eval interpreter has no equivalent
//! [`Value`] variant (see the cancelled `eval-cptr-value` task), so
//! every method surfaces [`RuntimeError::Unsupported`] with a
//! pointer to the LLVM backend.
//!
//! Routing them through this dedicated handler (vs falling through
//! to [`RuntimeError::UnknownIntrinsic`]) keeps the dispatch table
//! exhaustive and makes the gap legible in the error message.

use crate::error::RuntimeError;
use crate::value::Value;

const METHODS: &[&str] = &[
    "alloc",
    "free",
    "null",
    "null?",
    "offset",
    "read",
    "to_binary",
    "to_string",
    "write",
];

pub(super) fn matches_id(id: &str) -> bool {
    let Some(suffix) = id.strip_prefix("CPtr.") else {
        return false;
    };
    METHODS.contains(&suffix)
}

pub(super) fn dispatch(id: &str, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`{id}` is not implemented in the eval interpreter — \
             CPtr<T> values have no in-process representation. \
             Use `--backend=llvm` to exercise raw-pointer code paths.",
        ),
    })
}
