//! Global registry of every uniquely-named declaration in a program, keyed
//! by [`Identifier`].
//!
//! The registry is the authoritative gate that enforces "every identifier in
//! the program is unique." Insert sites are responsible for emitting a
//! diagnostic when [`GlobalRegistry::insert_struct`] (or its siblings) returns
//! an existing entry on collision -- the registry itself does not own
//! diagnostic emission, just collision detection.
//!
//! Today the registry only carries top-level structs, enums, and functions.
//! Methods on impls, enum variants, constants, protocols, and type aliases
//! will be added as the surrounding pipeline migrates onto path-based
//! [`Identifier`]s.

use std::collections::BTreeMap;

use expo_ast::identifier::Identifier;
use expo_ast::span::Span;

/// What kind of declaration an [`Identifier`] points at, plus the span of
/// the originating source-level decl (used for "already defined here"
/// diagnostic notes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalEntry {
    Struct { span: Span },
    Enum { span: Span },
    Function { span: Span },
}

impl GlobalEntry {
    /// Span of the source-level declaration that produced this entry.
    pub fn span(&self) -> Span {
        match self {
            GlobalEntry::Struct { span }
            | GlobalEntry::Enum { span }
            | GlobalEntry::Function { span } => *span,
        }
    }

    /// Human-readable kind label for diagnostics ("struct", "enum", ...).
    pub fn kind_label(&self) -> &'static str {
        match self {
            GlobalEntry::Struct { .. } => "struct",
            GlobalEntry::Enum { .. } => "enum",
            GlobalEntry::Function { .. } => "function",
        }
    }
}

/// Identifier-keyed map of every globally-named decl across the program.
#[derive(Clone, Debug, Default)]
pub struct GlobalRegistry {
    entries: BTreeMap<Identifier, GlobalEntry>,
}

impl GlobalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a struct decl. Returns the existing entry on collision so
    /// the caller can emit a "`X` is already defined" diagnostic with both
    /// spans. On success returns `None` and the new entry is stored.
    pub fn insert_struct(&mut self, id: Identifier, span: Span) -> Option<&GlobalEntry> {
        self.insert(id, GlobalEntry::Struct { span })
    }

    pub fn insert_enum(&mut self, id: Identifier, span: Span) -> Option<&GlobalEntry> {
        self.insert(id, GlobalEntry::Enum { span })
    }

    pub fn insert_function(&mut self, id: Identifier, span: Span) -> Option<&GlobalEntry> {
        self.insert(id, GlobalEntry::Function { span })
    }

    fn insert(&mut self, id: Identifier, entry: GlobalEntry) -> Option<&GlobalEntry> {
        if self.entries.contains_key(&id) {
            return self.entries.get(&id);
        }
        self.entries.insert(id, entry);
        None
    }

    pub fn get(&self, id: &Identifier) -> Option<&GlobalEntry> {
        self.entries.get(id)
    }

    /// Iterate every entry whose identifier lives in `pkg`. Tell-don't-ask
    /// query so callers never compare packages directly.
    pub fn iter_in_package<'a>(
        &'a self,
        pkg: &'a str,
    ) -> impl Iterator<Item = (&'a Identifier, &'a GlobalEntry)> {
        self.entries
            .iter()
            .filter(move |(id, _)| id.is_in_package(pkg))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
