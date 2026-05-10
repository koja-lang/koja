//! Per-backend dispatch table for `@extern "C"` function bodies on
//! the eval interpreter side. Mirrors [`crate::intrinsics`] in
//! shape: each registered extern is keyed by its C symbol name —
//! the same string the LLVM backend declares the function under
//! ([`expo_alpha_ir::IRExternAttrs::link_name`] when present, or
//! [`expo_alpha_ir::IRSymbol::last_segment`] otherwise) — and routed
//! to a hand-written handler that calls into `expo-runtime` over
//! the same C ABI symbol the LLVM backend would.
//!
//! Calling into the runtime via `extern "C"` (rather than re-
//! implementing the body in pure Rust) keeps eval byte-equivalent
//! with the LLVM backend by construction: both backends execute
//! the same machine code for the body.
//!
//! Adding a new extern: drop a sibling `<name>.rs` module that
//! `unsafe extern "C"`-declares the C symbol it wraps and exports
//! `pub(super) fn <handler>`, then register `(c_symbol, handler)`
//! in [`dispatch`]. Externs not in the table fall through with
//! `None` so the caller can surface
//! [`RuntimeError::ExternNotSupported`] with the alpha-side mangled
//! symbol attached for the diagnostic.

use crate::error::RuntimeError;
use crate::value::Value;

mod kernel;
mod time;

/// Run the registered extern under C symbol `link_name` against
/// `args`. Returns `None` when no handler is registered so the
/// caller can surface [`RuntimeError::ExternNotSupported`].
///
/// `strlen` traffics in `CPtr<UInt8>`, which the eval interpreter
/// has no [`Value`] variant for. It's listed here (rather than left
/// to fall through) so the error message points users at
/// `--backend=llvm` instead of "not registered" — keeps the gap
/// legible alongside the matching [`crate::intrinsics::cptr`]
/// handler.
pub(crate) fn dispatch(link_name: &str, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
    match link_name {
        "expo_time_now_millis" => Some(time::now_millis(args)),
        "expo_kernel_exit" => Some(kernel::exit(args)),
        "strlen" => Some(Err(RuntimeError::Unsupported {
            detail: format!(
                "extern \"C\" `{link_name}` returns / consumes a CPtr<UInt8>; \
                 the eval interpreter has no in-process pointer representation. \
                 Use `--backend=llvm` instead.",
            ),
        })),
        _ => None,
    }
}
