//! Per-function local-variable scope. Tracks `name -> `[`LocalId`] +
//! `LocalId -> `[`ResolvedType`] for params and `let`-introduced
//! bindings. Function-scoped today; block-scoped nesting is a
//! follow-up.

use std::collections::{BTreeMap, HashMap};

use expo_ast::identifier::{LocalId, ResolvedType};

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
    /// Replaces any previous declaration of the same name; the
    /// resolver routes both fresh declarations and same-type
    /// reassignments through this helper.
    pub(crate) fn declare(&mut self, name: &str, ty: ResolvedType) -> LocalId {
        let id = LocalId::new(self.next_id);
        self.next_id += 1;
        self.names.insert(name.to_string(), id);
        self.types.insert(id, ty);
        id
    }

    /// Look up `name` in scope; callers fall through to the global
    /// lookup on miss.
    pub(crate) fn lookup(&self, name: &str) -> Option<(LocalId, &ResolvedType)> {
        let id = self.names.get(name).copied()?;
        let ty = self
            .types
            .get(&id)
            .expect("LocalScope: name table points at id missing from type table");
        Some((id, ty))
    }
}
