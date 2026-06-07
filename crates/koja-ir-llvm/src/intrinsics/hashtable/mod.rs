//! Shared hashtable infrastructure for `Map<K, V>` and `Set<T>`.
//!
//! Both collections sit on the same open-addressed, linear-probing
//! table layout described in [`crate::types::hashtable_value_type`].
//! `Map`'s entry is a `(K, V)` pair; `Set`'s entry is a single `T`.
//! All the probe / resize / state-machine bookkeeping is identical
//! between them — only the entry size and the optional value-side
//! payload differ. This module owns the shared emitters; the per-
//! collection modules ([`super::map`], [`super::set`]) stitch them
//! into the per-method dispatch.
//!
//! # Submodule layout
//!
//! - [`util`]: low-level helpers ([`call_malloc`](util::call_malloc),
//!   [`resolve_hash_eq`](util::resolve_hash_eq),
//!   [`resolve_clone_fn`](util::resolve_clone_fn), …) used by every
//!   other submodule. Pure "build one instruction" wrappers — no
//!   per-method shape.
//! - [`lifecycle`]: allocate / inspect / duplicate — `new`, `length`,
//!   `empty?`, `from_map` identity, and `Map.clone` / `Set.clone`.
//! - [`read`]: the read-only probe loop and the `get` / `has?` /
//!   `remove` tails that consume it.
//! - [`resize`]: the load-factor check + rehash loop reused by every
//!   write path.
//! - [`insert`]: the write-side probe and the `Map.put` / `Set.insert`
//!   tails.
//! - [`from_list`]: `Set.from_list` and the inline `Set.insert` call
//!   it emits per element.
//!
//! Errors surface as typed [`LlvmError::Codegen`] values.

use koja_ir::{IRType, IRVariantTag};

mod from_list;
mod insert;
mod lifecycle;
mod read;
mod resize;
mod util;

pub(super) use from_list::emit_set_from_list;
pub(super) use insert::{emit_map_put, emit_set_insert};
pub(super) use lifecycle::{emit_empty_q, emit_identity, emit_length, emit_new, emit_table_clone};
pub(super) use read::{emit_has_q, emit_map_get, emit_remove};
pub(super) use util::ir_byte_size;

/// Initial bucket count for a freshly-allocated hashtable. Matches
/// v1's `INITIAL_CAPACITY` so eval / native / future JIT all
/// agree on the first-resize threshold.
pub(super) const INITIAL_CAPACITY: u64 = 8;

/// Per-byte state of a single bucket. `STATE_EMPTY` is `memset`-
/// produced (`0`) so a fresh malloc + memset routes through one
/// runtime call. `STATE_OCCUPIED` is "live entry, probe match
/// considers it". `STATE_TOMBSTONE` is "deleted entry, probe must
/// advance past it" — used by `remove` to keep the linear-probe
/// chain intact without back-shifting.
pub(super) const STATE_EMPTY: u64 = 0;
pub(crate) const STATE_OCCUPIED: u64 = 1;
pub(super) const STATE_TOMBSTONE: u64 = 2;

/// `Option<V>` variant tags as the stdlib declares them: `Some`
/// first (tag 0), then `None`. Map.get mints either flavour through
/// these so the numeric tags don't leak into the emitter body.
pub(super) const OPTION_SOME_TAG: IRVariantTag = IRVariantTag(0);
pub(super) const OPTION_NONE_TAG: IRVariantTag = IRVariantTag(1);

/// Per-instantiation layout knob for the per-method emitters. Set
/// passes `value_ty: None` (the entry is just `T`); Map passes
/// `Some(V)` (the entry is `K` followed by `V`).
pub(super) struct HashtableLayout<'ty> {
    pub entry_size: u64,
    pub key_size: u64,
    pub key_ty: &'ty IRType,
    pub value_ty: Option<&'ty IRType>,
}
