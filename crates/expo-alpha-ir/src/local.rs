//! [`IRLocalId`] — the IR-side handle for a local variable's storage
//! slot. Mirrors the [`crate::IRSymbol`] pattern at the value-flow
//! level: opaque to backends, minted only by the lower pass, with a
//! role-named `Display` view (`"local_N"`) so eval / LLVM / IR-text
//! snapshots can name slots without reaching into the integer.
//!
//! The translation seam is intentional. AST-level [`LocalId`]s carry
//! typecheck's per-function naming; IR-level `IRLocalId`s carry the
//! per-function slot identity backends key off. Today they're 1:1
//! integers — `from_local_id` just rewraps — but the seam exists so
//! a future pass (e.g. SSA-style local renumbering, or merging
//! parameters with body-declared locals into a single slot table)
//! can reshuffle the IR mapping without any AST or backend impact.

use std::fmt;

use expo_ast::identifier::LocalId;

/// IR-side handle for a local variable's storage slot. Constructed
/// only by the alpha lower pipeline ([`Self::from_local_id`]); every
/// downstream consumer (seal, eval, LLVM) reads but never mints.
///
/// Stored in [`crate::IRInstruction::LocalDecl`] /
/// [`crate::IRInstruction::LocalRead`] / [`crate::IRInstruction::LocalWrite`]
/// and on [`crate::IRFunctionParam::local_id`] so backends can key
/// `HashMap` / `BTreeMap` directly without inspecting the inner
/// integer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRLocalId(u32);

impl IRLocalId {
    /// Mint an [`IRLocalId`] from typecheck's [`LocalId`]. Crate-private:
    /// only the alpha lower pass stamps slot ids.
    ///
    /// Today the encoding is the raw integer round-trip; the helper
    /// exists so any future renumbering / re-indexing change lands
    /// in one place rather than at every lowering call site.
    pub(crate) fn from_local_id(local_id: LocalId) -> Self {
        Self(local_id.as_u32())
    }
}

impl fmt::Display for IRLocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "local_{}", self.0)
    }
}
