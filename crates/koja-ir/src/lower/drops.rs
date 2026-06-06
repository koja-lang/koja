//! Function-exit drop emission. Called at every site that ends the
//! function's control flow — explicit `return` and the synthesized
//! fall-through tail [`super::body::finalize_open_flow`].
//!
//! Drop insertion is deferred to the drop-glue pass. Under the current
//! value-semantics baseline every heap value leaks: a binding shared by
//! assignment (`b = a`) aliases the same payload, so freeing per-slot
//! at scope exit would double-free. The walk and the
//! [`crate::IRInstruction::DropLocal`] instruction remain as the
//! insertion point for that pass; today it emits nothing.

use crate::function::IRBlockId;

use super::ctx::FnLowerCtx;

/// Emit the function-exit drops for `block`. No-op under the leak
/// baseline (see module docs); kept as the seam the drop-glue pass
/// fills in.
pub(super) fn emit_function_exit_drops(_ctx: &mut FnLowerCtx, _block: IRBlockId) {}
