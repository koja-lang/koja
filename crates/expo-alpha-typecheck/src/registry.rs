//! Global registry of every uniquely-named declaration in a program,
//! keyed by [`GlobalRegistryId`] and reverse-indexed by [`Identifier`].
//!
//! The registry is the authoritative gate that enforces "every
//! identifier in the program is unique." Insert sites are responsible
//! for emitting a diagnostic when an insert returns
//! [`InsertOutcome::Collision`] -- the registry itself does not own
//! diagnostic emission, just collision detection.
//!
//! Today the registry only carries top-level structs, enums, functions,
//! and protocols. Methods on impls, enum variants, constants, and type
//! aliases will be added as the surrounding pipeline migrates onto
//! path-based [`Identifier`]s.
//!
//! Ids are assigned sequentially (monotonic `u32` counter). This is an
//! implementation detail; a future parallel-cache story will swap in
//! content-addressable hashing without changing the public surface.

use std::collections::HashMap;

use expo_ast::identifier::{GlobalRegistryId, Identifier};
use expo_ast::span::Span;

/// What kind of declaration a registry entry points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalKind {
    Enum,
    Function,
    Protocol,
    Struct,
}

impl GlobalKind {
    /// Human-readable kind label for diagnostics ("struct", "enum", ...).
    pub fn label(self) -> &'static str {
        match self {
            GlobalKind::Enum => "enum",
            GlobalKind::Function => "function",
            GlobalKind::Protocol => "protocol",
            GlobalKind::Struct => "struct",
        }
    }
}

/// A single registered declaration: the canonical [`Identifier`], its
/// [`GlobalKind`], and the source span of the originating decl (used
/// for "already defined here" diagnostic notes).
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub identifier: Identifier,
    pub kind: GlobalKind,
    pub span: Span,
}

/// Outcome of an insert attempt. `Fresh` means the id was newly minted;
/// `Collision` means an entry for the same identifier already existed
/// and the caller should emit a "already defined" diagnostic using
/// `existing`.
#[derive(Debug)]
pub enum InsertOutcome<'a> {
    Fresh(GlobalRegistryId),
    Collision { existing: &'a RegistryEntry },
}

/// Id-keyed registry of every globally-named decl across the program.
#[derive(Clone, Debug, Default)]
pub struct GlobalRegistry {
    entries: HashMap<GlobalRegistryId, RegistryEntry>,
    by_identifier: HashMap<Identifier, GlobalRegistryId>,
    next_id: u32,
}

impl GlobalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_enum(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Enum, span)
    }

    pub fn insert_function(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Function, span)
    }

    pub fn insert_protocol(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Protocol, span)
    }

    pub fn insert_struct(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Struct, span)
    }

    fn insert(
        &mut self,
        identifier: Identifier,
        kind: GlobalKind,
        span: Span,
    ) -> InsertOutcome<'_> {
        if let Some(&id) = self.by_identifier.get(&identifier) {
            let existing = self
                .entries
                .get(&id)
                .expect("reverse index points at a missing forward entry");
            return InsertOutcome::Collision { existing };
        }
        let id = GlobalRegistryId::new(self.next_id);
        self.next_id += 1;
        self.by_identifier.insert(identifier.clone(), id);
        self.entries.insert(
            id,
            RegistryEntry {
                identifier,
                kind,
                span,
            },
        );
        InsertOutcome::Fresh(id)
    }

    /// Forward lookup: dereference an id to its entry.
    pub fn get(&self, id: GlobalRegistryId) -> Option<&RegistryEntry> {
        self.entries.get(&id)
    }

    /// Reverse lookup: given an [`Identifier`], find its id (if any)
    /// and the entry. Used by resolve to stamp ids onto AST reference
    /// sites.
    pub fn lookup(&self, identifier: &Identifier) -> Option<(GlobalRegistryId, &RegistryEntry)> {
        let id = *self.by_identifier.get(identifier)?;
        let entry = self.entries.get(&id)?;
        Some((id, entry))
    }

    /// Iterate every entry in the registry. `HashMap` iteration order
    /// is not stable across runs; callers that need a deterministic
    /// order sort by id (matches declaration order under sequential
    /// assignment) or by `entry.identifier.qualified_name()`.
    pub fn iter(&self) -> impl Iterator<Item = (GlobalRegistryId, &RegistryEntry)> {
        self.entries.iter().map(|(id, entry)| (*id, entry))
    }

    /// Iterate every entry whose identifier lives in `pkg`. Tell-don't-ask
    /// query so callers never compare packages directly. Same stability
    /// caveat as [`Self::iter`].
    pub fn iter_in_package<'a>(
        &'a self,
        pkg: &'a str,
    ) -> impl Iterator<Item = (GlobalRegistryId, &'a RegistryEntry)> {
        self.entries
            .iter()
            .filter(move |(_, entry)| entry.identifier.is_in_package(pkg))
            .map(|(id, entry)| (*id, entry))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Compact tree-style registry rendering, used by
/// `expo alpha check --emit-ast` as a sidecar to the AST printer.
///
/// Format mirrors [`expo_ast::format_file`]: a header line with the
/// entry count, then one indented `<id> <kind> <qualified_name>
/// @<span>` line per entry. Entries are emitted in id order
/// (declaration order under sequential id assignment) so `<id>`
/// references from the AST printer line up one-to-one with rows here.
///
/// Always returns text that ends with `\n`. Empty registries render
/// just the header line.
pub fn format_registry(registry: &GlobalRegistry) -> String {
    use std::fmt::Write as _;

    let count = registry.len();
    let label = if count == 1 { "entry" } else { "entries" };
    let mut out = format!("Registry ({count} {label})\n");
    let mut rows: Vec<_> = registry.iter().collect();
    rows.sort_by_key(|(id, _)| *id);
    for (id, entry) in rows {
        writeln!(
            out,
            "  {id} {} {} @{}",
            entry.kind.label(),
            entry.identifier.qualified_name(),
            entry.span,
        )
        .expect("writing into a String cannot fail");
    }
    out
}
