//! Per-backend dispatch table for `@intrinsic` function bodies on
//! the eval interpreter side. Mirrors the LLVM backend's
//! `intrinsics/` shape — each registered intrinsic is keyed by its
//! [`expo_alpha_ir::FunctionKind::Intrinsic`] `id` (a stable
//! `Type.method` string) and routed to a hand-written handler.
//!
//! Adding a new intrinsic: drop a sibling `<name>.rs` module
//! exporting `pub(super) fn <name>`, register it in
//! [`handler_for`], and pin a 1-1 test in `tests/intrinsics.rs`.

use expo_alpha_ir::IRSymbol;

use crate::error::RuntimeError;
use crate::value::Value;

mod bitwise;
mod print;

use print::global_print;

/// Run the registered intrinsic `id` against `args`. The mangled
/// `symbol` is included only for the unknown-id error message so
/// users see the full call site, not just the dispatch id.
/// Unknown ids return [`RuntimeError::UnknownIntrinsic`] — a
/// missing registration fails loudly instead of silently returning
/// `Unit`.
pub(crate) fn dispatch(id: &str, symbol: &IRSymbol, args: &[Value]) -> Result<Value, RuntimeError> {
    if id == "print" {
        return global_print(args);
    }
    // 48-cell `Bitwise` family: `Int.band`, `UInt8.bsr`, ...
    // Routes here when the trailing segment is one of the six
    // ops; the handler branches on the parsed `(ty, op)` to
    // execute the right Rust shift/and/or/xor.
    if bitwise::parse_id(id).is_some() {
        return bitwise::dispatch(id, args);
    }
    Err(RuntimeError::UnknownIntrinsic {
        symbol: format!("{id} (at `{symbol}`)"),
    })
}
