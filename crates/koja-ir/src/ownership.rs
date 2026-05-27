//! Ownership classification for local-binding storage. Stamped
//! per-`LocalWrite` so the slot's *current* ownership is the most
//! recent write's ownership; the lowerer reads it back at scope-exit
//! drop emission to decide which slots need a `free`.
//!
//! Lives at the crate root rather than inside `lower::` because both
//! the IR vocabulary ([`crate::function::IRInstruction::LocalWrite`])
//! and the lowering pipeline ([`crate::lower`]) reference it; the
//! type-level decision belongs above the lowering helpers.
//!
//! Two variants only — `Owned` flags storage the slot must `free` at
//! scope exit; `Unowned` covers everything else (literals,
//! borrows, primitive copies). Per the foundation slice's "uniform
//! stamping" rule, the classifier returns `Owned` for any owning
//! site regardless of `T`'s kind; the drop-emission step filters
//! copy types via `IRType::is_copy()` so `move c: Int32` is
//! harmless.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ownership {
    Owned,
    Unowned,
}
