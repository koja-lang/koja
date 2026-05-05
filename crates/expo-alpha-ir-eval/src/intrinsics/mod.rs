//! Per-backend dispatch table for `@intrinsic` function bodies on
//! the eval interpreter side. Mirrors the LLVM backend's
//! `intrinsics/` shape — each registered intrinsic is keyed by its
//! full mangled symbol name and routed to a hand-written handler.
//!
//! Adding a new intrinsic: drop a sibling `<name>.rs` module
//! exporting `pub(super) fn <name>`, register it in
//! [`handler_for`], and pin a 1-1 test in `tests/intrinsics.rs`.

use crate::error::RuntimeError;
use crate::value::Value;

mod print;

use print::global_print;

/// Function pointer type for an intrinsic's interpreter handler.
type IntrinsicHandler = fn(&[Value]) -> Result<Value, RuntimeError>;

/// Run the registered intrinsic for `mangled` against `args`.
/// Unknown symbols return [`RuntimeError::UnknownIntrinsic`] — a
/// missing registration fails loudly instead of silently returning
/// `Unit`.
pub(crate) fn dispatch(mangled: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(handler) = handler_for(mangled) else {
        return Err(RuntimeError::UnknownIntrinsic {
            symbol: mangled.to_string(),
        });
    };
    handler(args)
}

fn handler_for(symbol: &str) -> Option<IntrinsicHandler> {
    match symbol {
        "Global.print" => Some(global_print),
        _ => None,
    }
}
