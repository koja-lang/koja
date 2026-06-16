//! IR-side handle for a local variable's storage slot. Opaque to
//! backends; only the lower pass mints. Display is `"local_N"`.

use std::fmt;

use koja_ast::identifier::LocalId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRLocalId(u32);

impl IRLocalId {
    /// Mint an [`IRLocalId`] from typecheck's [`LocalId`]. The lower
    /// pass is the only minter; downstream consumers read only.
    pub(crate) fn from_local_id(local_id: LocalId) -> Self {
        Self(local_id.as_u32())
    }

    /// The slot's raw index. Read-only escape hatch for post-lowering
    /// IR passes ([`crate::elaborate`]) that must mint a fresh slot one
    /// past every existing one in a function.
    pub(crate) fn as_u32(self) -> u32 {
        self.0
    }

    /// Placeholder slot id for synthesized glue
    /// ([`crate::FunctionKind::CloneGlue`] / `DropGlue`). Glue bodies
    /// read the operand straight off the parameter's SSA value
    /// (`params[0].id`) — the aggregate bodies `elaborate` synthesizes
    /// project fields off it, and the backend's collection bodies bind
    /// it via `get_nth_param` — so the param's `local_id` slot is
    /// never declared or read, and any id is sound here.
    pub(crate) fn synthetic_placeholder() -> Self {
        Self(0)
    }
}

impl fmt::Display for IRLocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "local_{}", self.0)
    }
}
