//! `Int.parse(input: String) -> Result<Int, String>` and
//! `Float.parse(input: String) -> Result<Float, String>`. The LLVM
//! backend stubs both with `unreachable` until the runtime parse
//! helpers land; the eval interpreter mirrors that — the
//! `Result<T, E>` enum payload would need registry handles to
//! materialize an `EnumPayload::Tuple` value, which the dispatch
//! seam doesn't carry. Surface [`RuntimeError::Unsupported`] so the
//! gap is explicit instead of failing as
//! [`RuntimeError::UnknownIntrinsic`].

use expo_alpha_ir::ParseTarget;

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(target: ParseTarget, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::Unsupported {
        detail: format!(
            "`{target:?}.parse` is not implemented in the eval interpreter yet — \
             matches the LLVM-side stub. Track via the parse runtime helpers.",
        ),
    })
}
