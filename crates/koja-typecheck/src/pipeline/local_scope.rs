//! Per-function local-variable scope. Maps each name to its
//! [`LocalId`] and each `LocalId` to its [`ResolvedType`] for params
//! and `let`-introduced bindings. One scope covers the whole
//! function. Blocks do not nest scopes.

use std::collections::{BTreeMap, HashMap};

use koja_ast::identifier::{LocalId, ResolvedType};

#[derive(Debug, Default)]
pub(crate) struct LocalScope {
    names: HashMap<String, LocalId>,
    types: BTreeMap<LocalId, ResolvedType>,
    next_id: u32,
}

impl LocalScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh [`LocalId`] for `name` and register its type.
    /// Replaces any previous declaration of the same name. Pattern
    /// bindings and closure params shadow through this helper.
    /// Assignment and destructuring reuse the existing id instead
    /// when the name is already in scope.
    pub(crate) fn declare(&mut self, name: &str, ty: ResolvedType) -> LocalId {
        let id = LocalId::new(self.next_id);
        self.next_id += 1;
        self.names.insert(name.to_string(), id);
        self.types.insert(id, ty);
        id
    }

    /// Mint a fresh nameless [`LocalId`] and register its type.
    /// Used for slots that are not reachable by name (wildcard
    /// closure params). The id participates in normal local
    /// allocation so IR lower can emit `LocalDecl` / `LocalWrite`
    /// the same way it does for named slots.
    pub(crate) fn declare_anonymous(&mut self, ty: ResolvedType) -> LocalId {
        let id = LocalId::new(self.next_id);
        self.next_id += 1;
        self.types.insert(id, ty);
        id
    }

    /// Look up `name` in scope. Callers fall through to the global
    /// lookup on miss.
    pub(crate) fn lookup(&self, name: &str) -> Option<(LocalId, &ResolvedType)> {
        let id = self.names.get(name).copied()?;
        let ty = self
            .types
            .get(&id)
            .expect("LocalScope name table points at id missing from type table");
        Some((id, ty))
    }

    /// Capture the visible name to id map so per-arm pattern bindings
    /// can be unwound at the arm boundary. Only names are saved. The
    /// id to type table and the `next_id` counter intentionally keep
    /// growing so any lowering / seal walk that reaches the popped
    /// binding's `LocalId` still sees its type.
    pub(crate) fn snapshot(&self) -> LocalScopeSnapshot {
        LocalScopeSnapshot {
            names: self.names.clone(),
        }
    }

    /// Restore the visible name to id map captured by [`Self::snapshot`].
    pub(crate) fn restore(&mut self, snapshot: LocalScopeSnapshot) {
        self.names = snapshot.names;
    }
}

/// Opaque token returned by [`LocalScope::snapshot`].
pub(crate) struct LocalScopeSnapshot {
    names: HashMap<String, LocalId>,
}
