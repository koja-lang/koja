//! The `IOReady` half of the [`super::delivery`] sub-pass.
//!
//! The reactor sends readiness events (`TAG_IO_READY = 2`, a bare
//! `IOReady{variant, fd}` payload). A business message `M` carries
//! them only as a union member, so delivery always wraps the payload
//! into that union.

use crate::types::IRType;

use super::delivery::{Injection, inject_union_member};

/// Mangled symbol of the kernel `IO.Ready` enum (`global/src/io.koja`).
/// Non-generic, so its symbol is the bare package-qualified name.
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

fn is_io_ready(member: &IRType) -> bool {
    matches!(member, IRType::Enum(symbol) if symbol.mangled() == IO_READY_SYMBOL)
}

/// The `IOReady` member of a union message `M` and the wrap that lifts
/// it into `M`. `None` when `M` is not a union with an `IOReady`
/// member.
pub(super) fn resolve(message_type: &IRType) -> Option<(IRType, Injection)> {
    inject_union_member(message_type, is_io_ready)
}
