//! `CString.to_string` — copies the bytes pointed at by the
//! CString's `ptr: CPtr<UInt8>` into a fresh Expo string. The eval
//! interpreter has no `CPtr` value variant (see the cancelled
//! `eval-cptr-value` task), so the conversion can't run on this
//! backend; surface [`RuntimeError::Unsupported`] with a pointer to
//! the LLVM backend instead of [`RuntimeError::UnknownIntrinsic`].

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn matches_id(id: &str) -> bool {
    id == "CString.to_string"
}

pub(super) fn dispatch(id: &str, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`{id}` is not implemented in the eval interpreter — \
             CString carries a CPtr<UInt8> with no in-process \
             representation. Use `--backend=llvm`.",
        ),
    })
}
