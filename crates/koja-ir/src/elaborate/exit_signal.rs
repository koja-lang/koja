//! The `ExitSignal` half of the [`super::delivery`] sub-pass.
//!
//! The runtime delivers a bare `Process.ExitSignal` payload
//! (`TAG_EXIT_SIGNAL = 4`) because the watcher's monomorphized `M`
//! (and its union tag layout) is unknowable runtime-side. Unlike
//! `IOReady`, `M` may also be *exactly* `ExitSignal`, in which case the
//! reshape skips the `UnionWrap`.

use crate::types::IRType;

use super::delivery::{Injection, inject_union_member};

/// Mangled symbol of the stdlib `Process.ExitSignal` struct
/// (non-generic, so the bare package-qualified name).
const EXIT_SIGNAL_SYMBOL: &str = "Global.Process.ExitSignal";

fn is_exit_signal(member: &IRType) -> bool {
    matches!(member, IRType::Struct(symbol) if symbol.mangled() == EXIT_SIGNAL_SYMBOL)
}

/// The `ExitSignal` type inside the message `M` and how it becomes
/// `M`: direct when `M` is `ExitSignal`, a union wrap when `M` is a
/// union with an `ExitSignal` member. `None` otherwise.
pub(super) fn resolve(message_type: &IRType) -> Option<(IRType, Injection)> {
    is_exit_signal(message_type)
        .then(|| (message_type.clone(), Injection::Direct))
        .or_else(|| inject_union_member(message_type, is_exit_signal))
}
