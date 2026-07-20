//! Function-exit drop emission. Called at every site that ends the
//! function's control flow: explicit `return` and the synthesized
//! fall-through tail [`super::body::finalize_open_flow`].
//!
//! Under the deep-copy-on-acquisition value-semantics baseline every
//! heap-leaf local owns an independent allocation (bindings, params,
//! and returns clone borrowed sources via [`super::ownership`]), so
//! each owning slot is freed unconditionally at scope exit with no
//! aliasing hazard. Composite heap is handled by the elaborate pass.

use crate::function::{IRBlockId, IRInstruction};

use super::ctx::FnLowerCtx;
use super::ownership::emit_slot_drops;

/// Emit the function-exit drops for `block`. Frees every owning
/// heap-leaf local slot, plus any owned match-subject temps this exit
/// escapes (a `return` inside an arm leaves before the arm tail's
/// subject release runs). Caller must have already materialized any
/// returned value, so the return clone is taken before its source
/// slot is dropped.
pub(super) fn emit_function_exit_drops(ctx: &mut FnLowerCtx, block: IRBlockId) {
    for value in ctx.subject_temps_since(0) {
        let ty = ctx.type_of(value);
        ctx.cfg
            .append(block, IRInstruction::DropValue { value, ty });
    }
    emit_slot_drops(ctx, block);
}
