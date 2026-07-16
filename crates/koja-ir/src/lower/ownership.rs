//! Value-semantics drop glue. Implements the reference-counting
//! baseline: every binding, parameter, and return *acquires* an owned
//! value ([`IRInstruction::Clone`]), so each owner holds storage it can
//! unconditionally release at scope exit ([`IRInstruction::DropLocal`]
//! / [`IRInstruction::DropValue`]).
//!
//! Lowering emits these `Clone` / `Drop` markers for every
//! [`IRType::is_heap_managed`] type. What each lowers to is decided
//! downstream:
//!
//! - **heap leaf** (`String` / `Binary` / `Bits`): an inline `rc++` /
//!   `rc--` on the `[i64 rc][i64 bit_length][payload]` block. Immutable
//!   blocks are shared rather than deep-copied, immortal rodata literals
//!   carry a sentinel rc so inc/dec are no-ops.
//! - **composite** (`List` / `Map` / `Set` / struct / enum / union /
//!   boxed `Indirect`): the [`crate::elaborate`] sub-pass rewrites the
//!   marker into a synthesized `clone_T` / `drop_T` call, or, for an
//!   all-`Copy` aggregate that needs no glue, the backend renders the
//!   `Clone` as a register copy and the `Drop` as a no-op.
//! - **closure** (`Function`): an inline `rc++` / `rc--` on the env
//!   block, the `rc--` running the body's capture-release glue at zero
//!   (see `crate::lower::closures` and `FunctionKind::DropClosureGlue`).
//!
//! Four lowering-side primitives:
//!
//! - [`materialize_owned`]: acquire a value at an ownership boundary.
//! - [`materialize_boundary_copy`]: deep-copy a value at a process
//!   boundary (send / spawn payloads).
//! - [`emit_slot_drops`]: release every heap-managed local at a
//!   control-flow exit.
//! - [`drop_discarded_temp`]: release an owned value whose statement
//!   result is thrown away.

use crate::function::{IRBlockId, IRFunctionParam, IRInstruction};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use super::ctx::FnLowerCtx;

/// Acquire `value` (typed `ty`) as an owned, independent value at an
/// ownership boundary: a binding, parameter promotion, or return.
///
/// - Non-heap-managed types pass through unchanged (scalars are
///   `Copy`).
/// - An already-owned value is *moved* (returned as-is).
/// - A borrowed heap-managed value (literal, `const`, slot/field read,
///   parameter) is *cloned* into a fresh owned value so the acquirer
///   gets storage it can drop without disturbing the source. For a
///   heap leaf the emitted `Clone` is an inline `rc++`. For a composite
///   the [`crate::elaborate`] pass rewrites it into a `clone_T` call
///   (or, for an all-`Copy` aggregate that needs no glue, the backend
///   renders it as a plain register copy).
///
/// The emitted `Clone` lands in `block`, before any sibling drop of
/// the source, so the copy is always taken while the source is live.
pub(super) fn materialize_owned(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    value: ValueId,
    ty: &IRType,
) -> ValueId {
    if !ty.is_heap_managed() || ctx.is_owned(value) {
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

/// Deep-copy `value` (typed `ty`) at a process boundary, i.e. a message
/// send or spawn-config payload. Unlike [`materialize_owned`], which
/// shares heap blocks with an `rc++`, the emitted
/// [`IRInstruction::DeepCopy`] produces a *physically* independent
/// value. Koja's rc bookkeeping is unsynchronized, so a payload that
/// aliases sender-owned blocks cannot cross to another process. The
/// copy happens unconditionally for heap-managed types, since even an owned
/// source may share storage with other live bindings.
///
/// The copy is marked owned and is *transferred*: the caller hands it
/// to the transport and never releases it (the runtime reclaims it via
/// the envelope drop glue). The source keeps its normal lifecycle.
pub(super) fn materialize_boundary_copy(
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    value: ValueId,
    ty: &IRType,
) -> ValueId {
    if !ty.is_heap_managed() {
        return value;
    }
    let copied = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::DeepCopy {
            dest: copied,
            source: value,
            ty: ty.clone(),
        },
    );
    ctx.mark_owned(copied);
    copied
}

/// Promote one function parameter into its local slot at the entry
/// block, returning the [`IRFunctionParam`] handle (whose `id` is the
/// incoming SSA parameter the backend binds the signature to).
///
/// Heap-leaf params are acquired (`rc++`) into their slot. The
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

/// Release every heap-managed local slot at a control-flow exit
/// `block` (function return / fall-through). Each slot owns its value
/// under value semantics, so the `Drop` is unconditional: a heap leaf
/// `rc--`s (freeing at zero), a composite is rewritten to a `drop_T`
/// call by [`crate::elaborate`] (or a no-op for an all-`Copy`
/// aggregate).
pub(super) fn emit_slot_drops(ctx: &mut FnLowerCtx, block: IRBlockId) {
    for (local, ty) in ctx.heap_managed_slots() {
        ctx.cfg
            .append(block, IRInstruction::DropLocal { local, ty });
    }
}

/// Free `value` in `block` when it owns a heap allocation that a
/// statement is about to discard (e.g. a bare `foo()` whose fresh
/// `String` or `List` result is unused). Borrowed or non-heap-managed
/// values are left untouched.
pub(super) fn drop_discarded_temp(ctx: &mut FnLowerCtx, block: IRBlockId, value: ValueId) {
    if !ctx.is_owned(value) {
        return;
    }
    let ty = ctx.type_of(value);
    if ty.is_heap_managed() {
        ctx.cfg
            .append(block, IRInstruction::DropValue { value, ty });
    }
}
