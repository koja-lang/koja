//! IR-side handle for a local variable's storage slot. Opaque to
//! backends; only the lower pass mints. Display is `"local_N"`.

use std::fmt;

use expo_ast::identifier::LocalId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRLocalId(u32);

impl IRLocalId {
    /// Mint an [`IRLocalId`] from typecheck's [`LocalId`]. The lower
    /// pass is the only minter; downstream consumers read only.
    pub(crate) fn from_local_id(local_id: LocalId) -> Self {
        Self(local_id.as_u32())
    }
}

impl fmt::Display for IRLocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "local_{}", self.0)
    }
}
