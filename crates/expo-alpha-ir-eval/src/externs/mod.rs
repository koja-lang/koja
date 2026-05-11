//! Per-backend dispatch table for `@extern "C"` function bodies on
//! the eval interpreter side. Mirrors [`crate::intrinsics`] in
//! shape: each registered extern is keyed by its C symbol name —
//! the same string the LLVM backend declares the function under
//! ([`expo_alpha_ir::IRExternAttrs::link_name`] when present, or
//! [`expo_alpha_ir::IRSymbol::last_segment`] otherwise) — and routed
//! to a hand-written handler that calls into `expo-runtime` (or
//! libc) over the same C ABI symbol the LLVM backend would.
//!
//! Modules in this folder mirror the stdlib source files that
//! declare the extern (`@extern "C"` shims live next to the methods
//! that call them in `lib/global/src/<name>.expo`), so a reader can
//! cross-reference one-to-one. Calling into the runtime via
//! `extern "C"` (rather than re-implementing the body in pure Rust)
//! keeps eval byte-equivalent with the LLVM backend by construction:
//! both backends execute the same machine code for the body.
//!
//! Adding a new extern: drop / extend the sibling `<name>.rs` module
//! matching the Expo source file, `unsafe extern "C"`-declare the C
//! symbol, export `pub(super) fn <handler>`, then register
//! `(c_symbol, handler)` in [`dispatch`]. Externs not in the table
//! fall through with `None` so the caller can surface
//! [`RuntimeError::ExternNotSupported`] with the alpha-side mangled
//! symbol attached for the diagnostic.

use crate::error::RuntimeError;
use crate::value::Value;

mod cptr;
mod kernel;
mod random;
mod time;

/// Run the registered extern under C symbol `link_name` against
/// `args`. Returns `None` when no handler is registered so the
/// caller can surface [`RuntimeError::ExternNotSupported`].
pub(crate) fn dispatch(link_name: &str, args: &[Value]) -> Option<Result<Value, RuntimeError>> {
    match link_name {
        "expo_kernel_exit" => Some(kernel::exit(args)),
        "expo_random_bytes" => Some(random::bytes(args)),
        "expo_random_int" => Some(random::int(args)),
        "expo_time_now_millis" => Some(time::now_millis(args)),
        "strlen" => Some(cptr::strlen_(args)),
        _ => None,
    }
}
