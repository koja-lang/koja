//! Heap-object header layout — the codegen-side single source of
//! truth for Koja's `[i64 rc][i64 bit_length][payload]` heap ABI.
//!
//! Every rc-managed leaf value (`String` / `Binary` / `Bits`) lives in
//! a block shaped `[i64 rc][i64 bit_length][payload bytes][NUL?]`. The
//! SSA pointer that flows through the IR addresses the **first payload
//! byte**; the `i64 bit_length` sits [`LENGTH_OFFSET`] before it and
//! the `i64 rc` sits [`HEADER_BYTES`] before it (at the block base).
//! So `koja_rc_inc`/`koja_rc_dec` and `free` recover the base via
//! `payload - HEADER_BYTES` ([`block_base`]) and a fresh `malloc`
//! derives its payload via `base + HEADER_BYTES` ([`payload_from_base`]).
//!
//! ## Reference counting (value-semantics baseline)
//!
//! `Clone` is an rc increment and `Drop` an rc decrement (freeing at
//! zero) — `MEMORY-MODEL.md`'s value-semantics model, made cheap by
//! sharing immutable blocks rather than deep-copying. The rc word is
//! the **first** word of every rc-managed block (uniform across leaf
//! and, later, collection/closure buffers), so one runtime primitive
//! pair operating on the block base serves every type; the only
//! per-type knowledge is the payload→base offset, which lives here.
//! Statically-allocated (rodata) literals carry a negative sentinel rc
//! ([`RC_IMMORTAL`]) so inc/dec are no-ops and they never reach `free`.
//!
//! This module exists so the header arithmetic lives in exactly one
//! place per crate. It is deliberately **not** shared with
//! `koja-runtime` via a common crate: the IR→backend boundary is a
//! sealed, serializable handoff and the runtime is a leaf
//! `staticlib`, so the ABI constants are mirrored there
//! (`koja-runtime`'s `util::{BLOCK_HEADER_SIZE, LENGTH_OFFSET}` and the
//! `rc < 0` immortal test). The two are an API contract kept in sync by
//! convention, not a shared dependency. `koja-ir`'s `types.rs` doc
//! comments are the authoritative human spec.

use inkwell::values::{IntValue, PointerValue};
use koja_ir::IRType;

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};

/// The leaf heap types backed by the single
/// `[i64 rc][i64 bit_length][payload]` block this module describes —
/// `String`, `Binary`, and `Bits`. These are the only types whose
/// `Clone` / `Drop` glue is a direct rc inc / dec on the block base;
/// composite heap is rewritten into per-type `clone_T` / `drop_T`
/// calls upstream.
pub(crate) fn is_heap_leaf(ty: &IRType) -> bool {
    matches!(ty, IRType::Binary | IRType::Bits | IRType::String)
}

/// Size in bytes of the length header that precedes every heap
/// payload. The SSA pointer addresses the first payload byte; the
/// `i64 bit_length` sits [`LENGTH_OFFSET`] before it; the `i64 rc`
/// sits `HEADER_BYTES` before it (at the block base).
///
/// API contract: MUST equal `koja-runtime`'s `util::BLOCK_HEADER_SIZE`.
pub(crate) const HEADER_BYTES: u64 = 16;

/// Distance in bytes from a payload pointer back to its `i64
/// bit_length` word. The `i64 rc` sits a further `LENGTH_OFFSET`
/// before that, at the block base ([`HEADER_BYTES`] before payload).
///
/// API contract: MUST equal `koja-runtime`'s `util::LENGTH_OFFSET`.
pub(crate) const LENGTH_OFFSET: u64 = 8;

/// Sentinel rc stamped into statically-allocated (rodata) payloads —
/// literals and `const`s. The runtime's `koja_rc_inc` / `koja_rc_dec`
/// treat any `rc < 0` as immortal: inc/dec are no-ops and the block is
/// never freed, so a literal payload never reaches `free` (it lives in
/// rodata, not the heap).
///
/// API contract: the runtime's immortal test is `rc < 0`; this is the
/// canonical negative value codegen writes.
pub(crate) const RC_IMMORTAL: i64 = i64::MIN;

/// `+HEADER_BYTES` as an `i64` constant — the payload offset from a
/// block base.
pub(crate) fn header_offset<'ctx>(ctx: &EmitContext<'ctx>) -> IntValue<'ctx> {
    ctx.context.i64_type().const_int(HEADER_BYTES, false)
}

/// `-HEADER_BYTES` as a signed `i64` constant — the block-base offset
/// from a payload pointer.
pub(crate) fn neg_header_offset<'ctx>(ctx: &EmitContext<'ctx>) -> IntValue<'ctx> {
    ctx.context
        .i64_type()
        .const_int((-(HEADER_BYTES as i64)) as u64, true)
}

/// GEP from a payload pointer back to its block base: `payload -
/// HEADER_BYTES`. This is the pointer to hand to `koja_rc_inc` /
/// `koja_rc_dec` (the `i64 rc` word lives here); the `i64 bit_length`
/// sits [`LENGTH_OFFSET`] after it.
///
/// A null payload selects a null base rather than the wrapped
/// `0 - HEADER_BYTES` address, so the (null-safe) runtime rc
/// primitives no-op. Local slots are zero-initialized at `LocalDecl`,
/// making "drop a never-written slot" a legal path — e.g. a `receive`
/// arm's payload slot when a different arm matched.
pub(crate) fn block_base<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let raw_base = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg_header_offset(ctx)], name)
    }
    .or_ice()?;
    let is_null = ctx
        .builder
        .build_is_null(payload, &format!("{name}.is_null"))
        .or_ice()?;
    let null_base = raw_base.get_type().const_null();
    ctx.builder
        .build_select(is_null, null_base, raw_base, &format!("{name}.or_null"))
        .or_ice()
        .map(|v| v.into_pointer_value())
}

/// Load the `i64 bit_length` header for a heap payload (the word at
/// `payload - LENGTH_OFFSET`, between the rc word and the payload).
pub(crate) fn load_bit_length<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: PointerValue<'ctx>,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let neg_length = ctx
        .context
        .i64_type()
        .const_int((-(LENGTH_OFFSET as i64)) as u64, true);
    let length_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, payload, &[neg_length], &format!("{name}_len_ptr"))
            .or_ice()?
    };
    ctx.builder
        .build_load(ctx.context.i64_type(), length_ptr, name)
        .or_ice()
        .map(|value| value.into_int_value())
}

/// Initialize a freshly-`malloc`'d leaf heap block: store `rc = 1` at
/// the block base, the `bit_length` word [`LENGTH_OFFSET`] after it,
/// and return the payload pointer (`base + HEADER_BYTES`). The single
/// codegen site that stamps the `[i64 rc][i64 bit_length]` header —
/// every inline block builder (`Concat`, clone, `CPtr`/`CString`)
/// routes its header write through here so the rc word is never
/// forgotten.
pub(crate) fn init_heap_block<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: PointerValue<'ctx>,
    bit_length: IntValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    ctx.builder
        .build_store(base, i64_ty.const_int(1, false))
        .or_ice()?;
    let length_ptr = payload_from_length_base(ctx, base, name)?;
    ctx.builder.build_store(length_ptr, bit_length).or_ice()?;
    payload_from_base(ctx, base, name)
}

/// GEP from a block base to its `i64 bit_length` word: `base +
/// LENGTH_OFFSET` (the slot between the rc word and the payload).
fn payload_from_length_base<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let length_off = ctx.context.i64_type().const_int(LENGTH_OFFSET, false);
    unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, base, &[length_off], &format!("{name}_len"))
    }
    .or_ice()
}

/// GEP from a freshly-allocated block base to its payload pointer:
/// `base + HEADER_BYTES`.
pub(crate) fn payload_from_base<'ctx>(
    ctx: &EmitContext<'ctx>,
    base: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    unsafe {
        ctx.builder
            .build_in_bounds_gep(i8_ty, base, &[header_offset(ctx)], name)
    }
    .or_ice()
}

/// Total block size for a heap value with `body_bytes` of payload:
/// `body_bytes + HEADER_BYTES (+ 1 if `with_nul`)`. Centralizes the
/// header/NUL arithmetic that `Concat` and `Clone` both open-code
/// (`with_nul` adds the trailing `\0` byte `String` keeps for libc
/// compatibility).
pub(crate) fn block_alloc_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    body_bytes: IntValue<'ctx>,
    with_nul: bool,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let overhead = HEADER_BYTES + u64::from(with_nul);
    ctx.builder
        .build_int_add(
            body_bytes,
            ctx.context.i64_type().const_int(overhead, false),
            name,
        )
        .or_ice()
}
