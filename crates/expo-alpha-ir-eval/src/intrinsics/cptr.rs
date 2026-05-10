//! `CPtr<T>` family — `alloc`, `free`, `null`, `null?`, `offset`,
//! `read`, `to_binary`, `to_string`, `write`. The LLVM backend mints
//! a real raw pointer; the eval interpreter has no equivalent
//! [`Value`] variant (see the cancelled `eval-cptr-value` task), so
//! every method surfaces [`RuntimeError::Unsupported`] with a
//! pointer to the LLVM backend.
//!
//! Routing them through this dedicated handler (vs falling through
//! to [`RuntimeError::UnknownIntrinsic`]) keeps the dispatch table
//! exhaustive and makes the gap legible in the error message.

use expo_alpha_ir::CPtrMethod;

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(method: CPtrMethod, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`CPtr.{method:?}` is not implemented in the eval interpreter — \
             CPtr<T> values have no in-process representation. \
             Use `--backend=llvm` to exercise raw-pointer code paths.",
        ),
    })
}
