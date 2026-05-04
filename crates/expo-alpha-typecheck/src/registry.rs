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
//!
//! # Function signatures
//!
//! Function signatures live inside the [`GlobalKind::Function`]
//! variant as `Option<FunctionSignature>`. The option encodes the
//! pipeline phase: `Function(None)` is the "collected but not yet
//! lifted" state; `Function(Some(sig))` is the "lifted" state reached
//! after `lift_signatures` runs. The variant-carried design makes
//! illegal states unrepresentable: `Struct`/`Enum`/`Protocol` entries
//! literally cannot carry a signature slot.

use std::collections::HashMap;

use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

/// A single resolved parameter: the surface-syntax name plus its
/// resolved type. Part of a [`FunctionSignature`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedParam {
    pub name: String,
    pub ty: ResolvedType,
}

/// A fully-resolved function signature: positional parameters plus
/// return type. Stamped onto a [`GlobalKind::Function`] entry by the
/// `lift_signatures` sub-pass.
///
/// Both params and return carry registry-backed [`ResolvedType`]s, so
/// a signature stays valid as long as its referent entries exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub params: Vec<ResolvedParam>,
    pub return_type: ResolvedType,
}

/// What kind of declaration a registry entry points at. Function
/// entries carry their signature inline (`None` until
/// `lift_signatures` stamps it in). Other kinds grow their own
/// per-variant metadata when features land (struct fields, enum
/// variants, protocol methods, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalKind {
    Enum,
    Function(Option<FunctionSignature>),
    Protocol,
    Struct,
}

impl GlobalKind {
    /// Human-readable kind label for diagnostics ("struct", "enum", ...).
    pub fn label(&self) -> &'static str {
        match self {
            GlobalKind::Enum => "enum",
            GlobalKind::Function(_) => "function",
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

    /// Seed a fresh registry with stdlib struct stubs for the scalar
    /// types alpha knows how to synthesize from literals (`Int`, `Bool`,
    /// `Unit`, `Float`, `String`). They register as ordinary
    /// [`GlobalKind::Struct`] entries under the `Global` package so
    /// alpha resolve has something to point at without special-casing
    /// primitives.
    ///
    /// Temporary scaffolding: once the real stdlib is formally compiled
    /// as a package, these entries land through the same `collect` path
    /// as any other decl and this constructor is no longer needed.
    /// Because stubs and real decls share the same shape, swapping in
    /// the real stdlib requires no changes to downstream consumers.
    pub fn with_stdlib_stubs() -> Self {
        let mut reg = Self::default();
        for name in ["Int", "Bool", "Unit", "Float", "String"] {
            let outcome = reg.insert_struct(
                Identifier::new("Global", vec![name.to_string()]),
                Span::default(),
            );
            debug_assert!(
                matches!(outcome, InsertOutcome::Fresh(_)),
                "stdlib stub `Global.{name}` collided on preload — registry was not empty",
            );
        }
        reg
    }

    pub fn insert_enum(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Enum, span)
    }

    /// Register a function in the `Function(None)` state. The
    /// signature is stamped in later by
    /// [`Self::set_signature`] from the `lift_signatures` sub-pass.
    pub fn insert_function(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Function(None), span)
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

    /// Stamp a resolved signature onto a function entry. Panics unless
    /// the entry's kind is exactly `Function(None)` — wrong kind or
    /// second set are compiler bugs in the sub-pass ordering.
    pub fn set_signature(&mut self, id: GlobalRegistryId, signature: FunctionSignature) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_signature on missing registry id {id} — collect invariant violation")
        });
        match &entry.kind {
            GlobalKind::Function(None) => {
                entry.kind = GlobalKind::Function(Some(signature));
            }
            GlobalKind::Function(Some(_)) => {
                panic!(
                    "set_signature called twice on `{}` — lift_signatures must stamp each \
                     function exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_signature called on non-function entry `{}` ({}) — \
                     only Function entries carry signatures",
                    entry.identifier,
                    other.label(),
                );
            }
        }
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
/// Function entries render their signature inline on the kind column
/// as `fn (p1: Global.Int, p2: Global.Int) -> Global.Int`. Functions
/// whose signature has not yet been lifted render as `fn <unlifted>`.
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
            format_kind(&entry.kind, registry),
            entry.identifier.qualified_name(),
            entry.span,
        )
        .expect("writing into a String cannot fail");
    }
    out
}

/// Render a [`GlobalKind`] for the registry sidecar. Function entries
/// get their signature inlined so the reader can see resolved param /
/// return types without chasing nested ids.
fn format_kind(kind: &GlobalKind, registry: &GlobalRegistry) -> String {
    match kind {
        GlobalKind::Enum => "enum".to_string(),
        GlobalKind::Function(None) => "fn <unlifted>".to_string(),
        GlobalKind::Function(Some(sig)) => format_signature(sig, registry),
        GlobalKind::Protocol => "protocol".to_string(),
        GlobalKind::Struct => "struct".to_string(),
    }
}

fn format_signature(sig: &FunctionSignature, registry: &GlobalRegistry) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, format_resolved(&p.ty, registry)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "fn ({params}) -> {}",
        format_resolved(&sig.return_type, registry),
    )
}

fn format_resolved(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    let head = match ty.resolution {
        Resolution::Unresolved => "<unresolved>".to_string(),
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.qualified_name(),
            None => format!("<id {id}>"),
        },
    };
    if ty.type_args.is_empty() {
        head
    } else {
        let args = ty
            .type_args
            .iter()
            .map(|arg| format_resolved(arg, registry))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{head}<{args}>")
    }
}
