//! Value-semantics drop glue for the heap-leaf types (`String` /
//! `Binary` / `Bits`). Implements the reference-counting baseline:
//! every binding, parameter, and return *acquires* an owned reference
//! (`Clone` = `rc++`), so each owner holds a reference it can
//! unconditionally release at scope exit (`Drop` = `rc--`, freeing at
//! zero) — immutable blocks are shared rather than deep-copied, and
//! immortal rodata literals carry a sentinel rc so inc/dec are no-ops.
//!
//! Three lowering-side primitives:
//!
//! - [`materialize_owned`] — acquire a value at an ownership boundary,
//!   emitting a [`IRInstruction::Clone`] (`rc++`) when the source is
//!   borrowed.
//! - [`emit_slot_drops`] — release every heap-leaf local at a
//!   control-flow exit ([`IRInstruction::DropLocal`], `rc--`).
//! - [`drop_discarded_temp`] — release an owned heap-leaf value whose
//!   statement result is thrown away ([`IRInstruction::DropValue`]).
//!
//! Composite heap (`List` / `Map` / `Set` / structs / enums /
//! closures) is out of scope here: those are routed through the
//! `elaborate` sub-pass's synthesized `clone_T` / `drop_T`.

use crate::function::{IRBlockId, IRFunctionParam, IRInstruction};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use super::ctx::FnLowerCtx;

/// Leaf heap types backed by a single `[i64 rc][i64 bit_length]
/// [payload]` block — the only types the leaf-phase drop glue rc-incs
/// and rc-decs inline. Composite heap is handled by the elaborate pass.
pub(super) fn is_heap_leaf(ty: &IRType) -> bool {
    matches!(ty, IRType::Binary | IRType::Bits | IRType::String)
}

/// Types lowering acquires on binding and releases at scope exit —
/// the heap leaves plus every composite that *might* own heap storage
/// (collections, boxed-recursive `Indirect`, closures, and any struct
/// / enum / union, conservatively). This predicate is intentionally
/// pure (no program access): generic struct / enum instantiations
/// don't exist yet during per-package lowering, so the precise
/// "does this aggregate actually own heap" question is deferred to
/// the post-merge [`crate::elaborate`] pass via
/// [`crate::elaborate::needs_drop`].
///
/// The conservatism is safe and cheap: a composite that turns out to
/// be all-`Copy` (e.g. `struct Point { x: Int, y: Int }`) gets no glue
/// registered, so the backend renders its `Clone` as a register copy
/// and its `Drop` as a no-op — identical to never having emitted them.
pub(super) fn is_heap_managed(ty: &IRType) -> bool {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => true,
        IRType::Enum(_)
        | IRType::Indirect(_)
        | IRType::List(_)
        | IRType::Map { .. }
        | IRType::Set(_)
        | IRType::Struct(_)
        | IRType::Union { .. } => true,
        // Closures (`Function`) own a heap env, but their clone / drop
        // glue needs the per-instance capture layout, which the
        // structural `IRType::Function` doesn't carry. They keep their
        // existing closure-specific drop path until the closure-glue
        // slice (plan phase 3) wires capture-recursive release.
        IRType::Function { .. }
        | IRType::Bool
        | IRType::CPtr(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => false,
    }
}

/// Acquire `value` (typed `ty`) as an owned, independent value at an
/// ownership boundary — a binding, parameter promotion, or return.
///
/// - Non-heap-leaf types pass through unchanged (scalars are `Copy`;
///   composites are the elaborate pass's job).
/// - An already-owned heap-leaf value is *moved* (returned as-is).
/// - A borrowed heap-leaf value (literal, `const`, slot/field read,
///   parameter) is *cloned* into a fresh owned allocation so the
///   acquirer gets storage it can drop without disturbing the source.
///
/// The emitted `Clone` lands in `block`, before any sibling drop of
/// the source, so the copy is always taken while the source is live.
pub(super) fn materialize_owned(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    value: ValueId,
    ty: &IRType,
) -> ValueId {
    if !is_heap_leaf(ty) || ctx.is_owned(value) {
        return value;
    }
    let cloned = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::Clone {
            dest: cloned,
            source: value,
            ty: ty.clone(),
        },
    );
    ctx.mark_owned(cloned);
    cloned
}

/// Promote one function parameter into its local slot at the entry
/// block, returning the [`IRFunctionParam`] handle (whose `id` is the
/// incoming SSA parameter the backend binds the signature to).
///
/// Heap-leaf params are acquired (`rc++`) into their slot: the
/// caller's argument is only borrowed, so the slot must own its own
/// reference that this frame's exit drops can release (`rc--`) without
/// freeing the caller's value out from under it. Shared by every
/// fn-param promotion site (named fns, closures, fn-as-value wrappers)
/// so the acquire-on-acquisition rule holds uniformly.
pub(super) fn promote_param(
    ctx: &mut FnLowerCtx,
    entry: IRBlockId,
    local: IRLocalId,
    ty: IRType,
) -> IRFunctionParam {
    let id = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        entry,
        IRInstruction::LocalDecl {
            local,
            ty: ty.clone(),
        },
    );
    let stored = materialize_owned(ctx, entry, id, &ty);
    ctx.cfg.append(
        entry,
        IRInstruction::LocalWrite {
            local,
            value: stored,
        },
    );
    ctx.mark_local_declared(local, ty.clone());
    IRFunctionParam {
        id,
        local_id: local,
        ty,
    }
}

/// Release every heap-leaf local slot at a control-flow exit `block`
/// (function return / fall-through). Each slot owns a reference under
/// the rc baseline, so the `rc--` is unconditional (freeing the block
/// only when its count hits zero).
pub(super) fn emit_slot_drops(ctx: &mut FnLowerCtx, block: IRBlockId) {
    for (local, ty) in ctx.heap_leaf_slots() {
        ctx.cfg
            .append(block, IRInstruction::DropLocal { local, ty });
    }
}

/// Free `value` in `block` when it owns a heap-leaf allocation that a
/// statement is about to discard (e.g. a bare `foo()` whose fresh
/// `String` result is unused). Borrowed or non-heap-leaf values are
/// left untouched.
pub(super) fn drop_discarded_temp(ctx: &mut FnLowerCtx, block: IRBlockId, value: ValueId) {
    if !ctx.is_owned(value) {
        return;
    }
    let ty = ctx.type_of(value);
    if is_heap_leaf(&ty) {
        ctx.cfg
            .append(block, IRInstruction::DropValue { value, ty });
    }
}
