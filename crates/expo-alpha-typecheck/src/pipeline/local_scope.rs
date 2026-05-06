//! Per-function local-variable scope.
//!
//! [`LocalScope`] tracks the bindings introduced in a single function:
//! every parameter (pre-populated by `resolve_function` before walking
//! the body) and every `let`-introduced variable (declared by
//! `Statement::Assignment` resolution). Each binding gets a unique
//! [`LocalId`]; references to the binding stamp [`Resolution::Local`]
//! with that id so downstream passes (seal, IR lower, eval, codegen)
//! agree on which slot a name refers to.
//!
//! Function-scoped semantics: a single [`LocalScope`] covers the whole
//! function body, including any nested `if` / `unless` arms.
//! Block-scoped lexical nesting is a follow-up slice — once it lands,
//! `LocalScope` grows a stack of frames; until then names introduced
//! inside an arm leak into the outer function scope.

use std::collections::{BTreeMap, HashMap};

use expo_ast::identifier::{LocalId, ResolvedType};

/// Function-scoped binding registry.
#[derive(Debug, Default)]
pub(crate) struct LocalScope {
    /// `name -> LocalId` for the most recent declaration of `name`
    /// in this function. Reassignment reuses the existing id (the
    /// resolver checks the assignment is type-compatible before
    /// touching this map).
    names: HashMap<String, LocalId>,
    /// `LocalId -> declared type`. Populated when `declare` mints
    /// the id; never overwritten afterward (reassignment with a
    /// different type is rejected before reaching `update`).
    types: BTreeMap<LocalId, ResolvedType>,
    /// Counter for the next id this scope will mint. Starts at 0 so
    /// param ids land in declaration order: the first param is
    /// `LocalId(0)`, the second `LocalId(1)`, and so on.
    next_id: u32,
}

impl LocalScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a fresh binding for `name` with declared type `ty`.
    /// Returns the minted [`LocalId`] so the caller can stamp it onto
    /// the AST node that introduced the binding (e.g. `Param.local_id`
    /// for params, the assignment-target Ident's resolution for `let`s).
    ///
    /// Replaces any previous declaration of the same name in scope.
    /// Today the resolver routes both fresh declarations and same-type
    /// reassignments through this helper; once block scoping lands,
    /// the resolver will distinguish "shadow" from "rebind".
    pub(crate) fn declare(&mut self, name: &str, ty: ResolvedType) -> LocalId {
        let id = LocalId::new(self.next_id);
        self.next_id += 1;
        self.names.insert(name.to_string(), id);
        self.types.insert(id, ty);
        id
    }

    /// Look up `name` in scope. Returns `(LocalId, declared type)` on
    /// hit; `None` on miss. Callers fall through to the global lookup
    /// when this misses.
    pub(crate) fn lookup(&self, name: &str) -> Option<(LocalId, &ResolvedType)> {
        let id = self.names.get(name).copied()?;
        let ty = self
            .types
            .get(&id)
            .expect("LocalScope: name table points at id missing from type table");
        Some((id, ty))
    }

    /// Look up the declared type of an existing [`LocalId`]. Used by
    /// the IR lower path to size `LocalDecl { ty }` instructions
    /// without re-querying the registry.
    #[allow(dead_code)] // consumed by IR lower in a later todo
    pub(crate) fn type_of(&self, id: LocalId) -> Option<&ResolvedType> {
        self.types.get(&id)
    }
}
