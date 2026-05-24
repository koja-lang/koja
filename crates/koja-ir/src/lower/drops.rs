//! Function-exit drop emission. Emitted at every site that ends
//! the function's control flow — explicit `return` and the
//! synthesized fall-through tail [`super::body::finalize_open_flow`]
//! stamps when the function body falls off the end without an
//! explicit terminator.
//!
//! The model is function-flat: IR locals don't track
//! per-block scopes, so every Live & Owned slot at the function-exit
//! site gets a [`crate::IRInstruction::DropLocal`]. This is
//! equivalent to v1's `emit_function_drops` for the foundation
//! slice and consistent with the "drops at function boundary" Phase A
//! contract from `COMPILER-NORTHSTAR.md`.
//!
//! Drop order isn't load-bearing today: the pipeline does not yet expose any
//! observable side effect from `free` (no destructor methods, no
//! reference cycles). [`FnLowerCtx::live_owned_locals`] yields
//! slots in declaration order (the [`std::collections::BTreeMap`]
//! orders by `IRLocalId`, which is monotonically allocated by
//! [`koja_ast::identifier::LocalId`]); this is stable enough for
//! tests to pin against. Future destructor or finalizer work would
//! revisit ordering.

use crate::function::{IRBlockId, IRInstruction};

use super::ctx::FnLowerCtx;

/// Append a [`crate::IRInstruction::DropLocal`] for every Live &
/// Owned slot in `ctx` to `block`. Order matches
/// [`FnLowerCtx::live_owned_locals`] (declaration order). Caller
/// is responsible for stamping the function-exit terminator on
/// `block` *after* this call so the drops sit in the block before
/// the terminator.
pub(super) fn emit_function_exit_drops(ctx: &mut FnLowerCtx, block: IRBlockId) {
    let drops: Vec<IRInstruction> = ctx
        .live_owned_locals()
        .map(|(local, ty)| IRInstruction::DropLocal { local, ty })
        .collect();
    for drop in drops {
        ctx.cfg.append(block, drop);
    }
}
