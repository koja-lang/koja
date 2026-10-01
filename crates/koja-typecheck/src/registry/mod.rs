//! Global registry of every uniquely-named declaration, keyed by
//! [`GlobalRegistryId`] and reverse-indexed by [`Identifier`]. The
//! registry is the authoritative gate enforcing identifier uniqueness.
//! Insert sites emit the "already defined" diagnostic when an insert
//! returns [`InsertOutcome::Collision`].
//!
//! Structs, enums, functions, protocols, constants, and type aliases
//! register, each under its path-based [`Identifier`]. Methods
//! register as functions under `[target_path, method_name]`.
//!
//! Ids are assigned sequentially from a monotonic `u32` counter.
//!
//! # Function signatures
//!
//! [`GlobalKind::Function`] carries [`FunctionDefinition`], whose
//! signature is `None` after collect and `Some` after
//! `lift_signatures`. Its arity and origin are function-only metadata,
//! so non-function entries cannot carry them.
//!
//! Registry rendering for `koja check --emit-ast` lives in the
//! [`format`] submodule. It is a separate concern from the data and
//! insert API, with a different audience (diagnostic rendering rather
//! than pipeline work).

use std::collections::{BTreeMap, HashMap, HashSet};

use koja_ast::ast::{
    BuiltinDecl, Constant, EnumDecl, Function, Literal, Name, ProtocolDecl, StructDecl, TypeAlias,
    TypeParam, name_texts,
};
use koja_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

mod candidates;
mod conformance;
mod definitions;
mod format;

pub use candidates::{Candidate, CandidateDetail, CandidateKind, KEYWORDS};
pub use definitions::{
    BoundOverlay, BuiltinDefinition, BuiltinShape, Conformance, ConformanceScope,
    ConstantDefinition, Dispatch, EnumDefinition, FunctionDefinition, FunctionSignature,
    ProtocolDefinition, ResolvedEnumVariant, ResolvedParam, ResolvedProtocolBound,
    ResolvedProtocolMethod, ResolvedStructField, ResolvedVariantData, StructDefinition,
};
pub use format::format_registry;
pub use koja_ast::ast::FunctionOrigin;

/// What kind of declaration a registry entry points at.
///
/// Most variants carry their lifted payload inline as `Option<_>`:
/// `None` is the "collected but not yet lifted" state, `Some(_)` the
/// lifted state reached after `lift_signatures` runs. Stdlib
/// primitives land pre-stamped (`Struct(Some(empty_def))`) so
/// `record_conformance` against them works the same as against
/// user-declared structs. [`GlobalKind::Constant`] boxes its
/// `Some(_)` payload so this enum stays a reasonable size despite
/// the large [`ConstantDefinition`] (AST-valued) shape.
///
/// Trait `impl P for T` blocks do *not* get their own registry
/// entry kind. Their methods register on `[target_head, method]`
/// like inherent / inline methods, and the conformance fact
/// (`T : P`) lives on `T`'s [`StructDefinition`] /
/// [`EnumDefinition`] `conformances` field. This keeps the
/// receiver entry self-contained for IR. See
/// [`StructDefinition::conformances`] for the full rationale.
#[derive(Clone, Debug)]
pub enum GlobalKind {
    /// A compiler-owned type declared with the `builtin` keyword.
    /// No `Option` lifecycle because the shape is stamped at seed time.
    Builtin(BuiltinDefinition),
    Constant(Option<Box<ConstantDefinition>>),
    Enum(Option<EnumDefinition>),
    Function(FunctionDefinition),
    Protocol(Option<ProtocolDefinition>),
    Struct(Option<StructDefinition>),
    /// `type X = ...` declared at top level. The `Option` mirrors
    /// other lifecycle-payload variants, `None` after collect and
    /// `Some(expansion)` after `lift_type_aliases` resolves the RHS.
    /// The expansion is the canonical [`ResolvedType`] the alias
    /// stands for. For the surface-aliasing case
    /// (`type Pet = Cat | Dog | Fish`) that is typically a
    /// canonical [`ResolvedType::Union`], but any `ResolvedType`
    /// shape is permissible.
    TypeAlias(Option<ResolvedType>),
}

impl GlobalKind {
    pub fn label(&self) -> &'static str {
        match self {
            GlobalKind::Builtin(_) => "builtin",
            GlobalKind::Constant(_) => "constant",
            GlobalKind::Enum(_) => "enum",
            GlobalKind::Function(_) => "function",
            GlobalKind::Protocol(_) => "protocol",
            GlobalKind::Struct(_) => "struct",
            GlobalKind::TypeAlias(_) => "type alias",
        }
    }
}

/// A single registered declaration, with its canonical [`Identifier`],
/// [`GlobalKind`], source spans, and any generic-decl param names
/// declared on it.
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    /// `@deprecated` message, always non-empty. `None` means not
    /// deprecated.
    pub deprecation: Option<String>,
    /// Canonical path-based name.
    pub identifier: Identifier,
    /// Declaration kind and its lifted payload.
    pub kind: GlobalKind,
    /// Span of the name token alone, so a diagnostic or an editor can
    /// point at the name.
    pub name_span: Span,
    /// Span of the whole declaration.
    pub span: Span,
    /// Generic param names, stamped at collect time directly from the
    /// AST so [`GlobalRegistry::type_params`] is queryable mid-lift,
    /// before [`StructDefinition`], [`EnumDefinition`], and signature
    /// payloads are stamped.
    pub type_params: Vec<String>,
    /// Parallel to `type_params` (same length, same indexing). Each
    /// inner vector holds the resolved protocol bounds from a
    /// `<T: P1 & P2>` bound, in source order. An empty inner vector
    /// means the param is unbounded. Collect stores one empty inner
    /// vector per param. The bounds-resolve sub-pass of lift replaces
    /// it via [`GlobalRegistry::set_type_param_bounds`].
    pub type_param_bounds: Vec<Vec<ResolvedProtocolBound>>,
    /// The `priv` enforcement scope. See [`VisibilityScope`] for the
    /// three-case rationale. Functions can be `TypePrivate`. Every
    /// other entry kind is either `Public` or `PackagePrivate`.
    pub visibility: VisibilityScope,
}

impl RegistryEntry {
    /// The function payload, or `None` when the entry is not a function.
    pub fn function_definition(&self) -> Option<&FunctionDefinition> {
        match &self.kind {
            GlobalKind::Function(definition) => Some(definition),
            _ => None,
        }
    }

    /// The function payload of an entry the caller already proved is a
    /// function (e.g. it came from [`GlobalRegistry::function_lookup`]).
    pub fn expect_function_definition(&self) -> &FunctionDefinition {
        self.function_definition().unwrap_or_else(|| {
            panic!(
                "`{}` is a {}, not a function",
                self.identifier,
                self.kind.label()
            )
        })
    }

    /// The lifted signature of a proven function entry. Panics when
    /// `lift_signatures` has not stamped the signature yet.
    pub fn expect_function_signature(&self) -> &FunctionSignature {
        self.expect_function_definition()
            .signature
            .as_ref()
            .unwrap_or_else(|| {
                panic!(
                    "function `{}` has no lifted signature; lift_signatures must run first",
                    self.identifier
                )
            })
    }
}

/// Typecheck-internal projection of the AST [`koja_ast::ast::Visibility`]
/// plus the contextual scope where a `priv` decl appeared. Encoded as
/// a single enum so illegal states (public-with-owner, private-with-no-
/// scope) are unrepresentable. The surface keyword and its declaration
/// position together pick exactly one variant.
///
/// - `Public` (default): no restriction. The reference resolves
///   wherever the name is reachable.
/// - `PackagePrivate`: any top-level `priv` decl (function, struct,
///   enum, constant, type alias, protocol). Usable from any file
///   in the same package. The package name lives on the entry's
///   [`Identifier`] so it does not need to be repeated here.
/// - `TypePrivate(type_id)`: `priv fn` declared inside a `struct` /
///   `enum` / `impl` body. Callable only from other methods on the
///   same target type, including across inherent and protocol-impl
///   blocks, since they all register at `[type, method]` and share
///   one owner id. Only functions can be type-private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibilityScope {
    Public,
    PackagePrivate,
    TypePrivate(GlobalRegistryId),
}

/// Outcome of an insert attempt. `Collision` carries the existing
/// entry so the caller can emit an "already defined" diagnostic.
#[derive(Debug)]
pub(crate) enum InsertOutcome<'a> {
    Collision { existing: &'a RegistryEntry },
    Fresh(GlobalRegistryId),
}

/// Outcome of a successful [`GlobalRegistry::claim_builtin_stub`].
/// On `ArityMismatch` the stub is still consumed (span stamped, no
/// second claim) but keeps its seeded param names, so collect can
/// diagnose without leaving a half-adopted entry behind.
#[derive(Debug)]
pub(crate) enum ClaimOutcome {
    ArityMismatch {
        id: GlobalRegistryId,
        expected_arity: usize,
    },
    Claimed(GlobalRegistryId),
}

/// Id-keyed registry of every globally-named decl across the program.
#[derive(Clone, Debug, Default)]
pub struct GlobalRegistry {
    entries: HashMap<GlobalRegistryId, RegistryEntry>,
    by_identifier: HashMap<Identifier, NameEntries>,
    next_id: u32,
    /// Seeded builtin stubs not yet claimed by a `builtin`
    /// declaration. [`Self::claim_builtin_stub`] drains it.
    unclaimed_builtin_stubs: HashSet<GlobalRegistryId>,
}

#[derive(Clone, Debug, Default)]
struct NameEntries {
    functions: BTreeMap<usize, GlobalRegistryId>,
    non_function: Option<GlobalRegistryId>,
}

/// Outcome of [`GlobalRegistry::function_lookup`].
pub enum FunctionLookup<'a> {
    Found(GlobalRegistryId, &'a RegistryEntry),
    /// No functions exist under this name.
    NoFunctions,
    /// Functions exist under this name, but none at the requested
    /// arity. Carries every declared arity, sorted ascending.
    WrongArity(Vec<usize>),
}

impl GlobalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a fresh registry with one [`GlobalKind::Builtin`] stub
    /// per compiler-owned type, all under the `Global` package so
    /// resolve never special-cases them. `Option<T>` is *not*
    /// stubbed. It is an ordinary enum in autoimported
    /// `Global.kernel`.
    ///
    /// Each stub carries its [`BuiltinShape`] and an empty
    /// conformance map, so `impl P for Int` blocks register
    /// conformances the same way they do against user structs. A
    /// `builtin` declaration later claims the stub via
    /// [`Self::claim_builtin_stub`].
    pub(crate) fn with_stdlib_stubs() -> Self {
        use BuiltinShape as Shape;
        let mut reg = Self::default();
        let scalars = [
            ("Binary", Shape::Binary),
            ("Bits", Shape::Bits),
            ("Bool", Shape::Bool),
            ("Float", Shape::Float64),
            ("Float32", Shape::Float32),
            ("Float64", Shape::Float64),
            ("Int", Shape::Int64),
            ("Int8", Shape::Int8),
            ("Int16", Shape::Int16),
            ("Int32", Shape::Int32),
            ("Int64", Shape::Int64),
            ("Never", Shape::Never),
            ("String", Shape::String),
            ("UInt8", Shape::UInt8),
            ("UInt16", Shape::UInt16),
            ("UInt32", Shape::UInt32),
            ("UInt64", Shape::UInt64),
            ("Unit", Shape::Unit),
        ];
        for (name, shape) in scalars {
            seed_builtin_stub(&mut reg, name, shape, Vec::new());
        }
        let type_param = |name: &str| vec![name.to_string()];
        seed_builtin_stub(&mut reg, "CPtr", Shape::CPtr, type_param("T"));
        seed_builtin_stub(&mut reg, "List", Shape::List, type_param("T"));
        seed_builtin_stub(
            &mut reg,
            "Map",
            Shape::Map,
            vec!["K".to_string(), "V".to_string()],
        );
        seed_builtin_stub(&mut reg, "Set", Shape::Set, type_param("T"));
        reg
    }

    /// Register a constant in the `Constant(None)` state. The
    /// resolved type + value [`ConstantDefinition`] is stamped in
    /// later by [`Self::set_constant_definition`].
    pub(crate) fn insert_constant(
        &mut self,
        identifier: Identifier,
        constant: &Constant,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Constant(None),
            constant.span,
            constant.name().span,
            Vec::new(),
            visibility,
        )
    }

    /// Register an enum in the `Enum(None)` state. The resolved
    /// variant roster is stamped in later by
    /// [`Self::set_enum_definition`]. The declared generic-param
    /// names are stored up front so resolve and lift can answer
    /// "what params are in scope inside this decl?" before the
    /// variant payload types have been resolved.
    pub(crate) fn insert_enum(
        &mut self,
        identifier: Identifier,
        decl: &EnumDecl,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Enum(None),
            decl.span,
            decl.name().span,
            type_param_names(&decl.type_params),
            visibility,
        )
    }

    /// Register a function in the `Function(None)` state. The
    /// signature is stamped in later by [`Self::set_signature`].
    /// Arity, origin, spans, and the function's own declared generic
    /// params (not the enclosing struct/impl's) are read from the AST
    /// node. Chained type-param scopes are rebuilt at resolve time.
    ///
    /// `visibility` captures the `priv fn` enforcement scope as a
    /// [`VisibilityScope`]: `Public` for default `fn`, or the
    /// `PackagePrivate` / `TypePrivate(type_id)` variant that
    /// matches where the `priv fn` was declared. See
    /// [`VisibilityScope`] for the mapping rule.
    pub(crate) fn insert_function(
        &mut self,
        identifier: Identifier,
        function: &Function,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        let arity = function.params.len();
        let names = self.by_identifier.entry(identifier.clone()).or_default();
        if let Some(id) = names
            .non_function
            .or_else(|| names.functions.get(&arity).copied())
        {
            return InsertOutcome::Collision {
                existing: self.entries.get(&id).expect("reverse index is valid"),
            };
        }
        let id = GlobalRegistryId::new(self.next_id);
        self.next_id += 1;
        names.functions.insert(arity, id);
        let type_params = type_param_names(&function.type_params);
        let type_param_bounds = vec![Vec::new(); type_params.len()];
        self.entries.insert(
            id,
            RegistryEntry {
                deprecation: None,
                identifier,
                kind: GlobalKind::Function(FunctionDefinition {
                    arity,
                    origin: function.origin,
                    signature: None,
                }),
                name_span: function.name.span,
                span: function.span,
                type_params,
                type_param_bounds,
                visibility,
            },
        );
        InsertOutcome::Fresh(id)
    }

    /// Register a protocol in the `Protocol(None)` state. Method
    /// roster is stamped later by [`Self::set_protocol_definition`].
    /// Unlike the other inserts, `type_params` comes from the caller.
    /// Collect builds the roster as `Self` followed by the declared
    /// names, and rejects a declared `Self` with a diagnostic.
    pub(crate) fn insert_protocol(
        &mut self,
        identifier: Identifier,
        decl: &ProtocolDecl,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Protocol(None),
            decl.span,
            decl.name().span,
            type_params,
            visibility,
        )
    }

    /// Register a struct in the `Struct(None)` state. The
    /// resolved field layout is stamped in later by
    /// [`Self::set_struct_definition`].
    pub(crate) fn insert_struct(
        &mut self,
        identifier: Identifier,
        decl: &StructDecl,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Struct(None),
            decl.span,
            decl.name().span,
            type_param_names(&decl.type_params),
            visibility,
        )
    }

    /// Register a `builtin` declaration that claimed no stub, as a
    /// `Struct(None)` entry. Collect uses this after reporting that
    /// the name is not a builtin, so the decl's methods still have an
    /// owner and a duplicate declaration collides as usual.
    pub(crate) fn insert_unclaimed_builtin(
        &mut self,
        identifier: Identifier,
        decl: &BuiltinDecl,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Struct(None),
            decl.span,
            decl.name().span,
            type_param_names(&decl.type_params),
            visibility,
        )
    }

    /// Register a `type X = ...` alias in the `TypeAlias(None)`
    /// state. The expansion is stamped in later by
    /// [`Self::set_type_alias_definition`]. Aliases take no generic
    /// params.
    pub(crate) fn insert_type_alias(
        &mut self,
        identifier: Identifier,
        alias: &TypeAlias,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::TypeAlias(None),
            alias.span,
            alias.name.span,
            Vec::new(),
            visibility,
        )
    }

    /// Stamp a `@deprecated` message onto an entry. Collect calls
    /// this at most once per decl, right after a fresh insert.
    pub(crate) fn set_deprecation(&mut self, id: GlobalRegistryId, message: String) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_deprecation on missing registry id {id}. This is a collect invariant violation"
            )
        });
        entry.deprecation = Some(message);
    }

    /// Stamp a resolved variant roster onto an enum entry. Panics
    /// unless the entry's kind is exactly `Enum(None)`.
    pub(crate) fn set_enum_definition(&mut self, id: GlobalRegistryId, definition: EnumDefinition) {
        self.stamp(id, "set_enum_definition", definition, |kind| match kind {
            GlobalKind::Enum(slot) => Some(slot),
            _ => None,
        });
    }

    /// Write `value` into the empty slot that `slot` selects on
    /// entry `id`. Every lift stamp goes through here, so the three
    /// invariant panics have one wording: `what` names the caller,
    /// a missing id is a collect bug, a `None` from `slot` is a
    /// wrong-kind entry, and a filled slot is a second stamp.
    fn stamp<T>(
        &mut self,
        id: GlobalRegistryId,
        what: &str,
        value: T,
        slot: impl FnOnce(&mut GlobalKind) -> Option<&mut Option<T>>,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("{what} on missing registry id {id}. This is a collect invariant violation")
        });
        let label = entry.kind.label();
        let Some(target) = slot(&mut entry.kind) else {
            panic!(
                "{what} called on {label} entry `{}`, which has no slot for this definition",
                entry.identifier,
            );
        };
        if target.is_some() {
            panic!(
                "{what} called twice on `{}`. The lift passes stamp each entry exactly once",
                entry.identifier,
            );
        }
        *target = Some(value);
    }

    /// Stamp a resolved method roster. Panics unless the entry's
    /// kind is exactly `Protocol(None)`.
    pub(crate) fn set_protocol_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: ProtocolDefinition,
    ) {
        self.stamp(
            id,
            "set_protocol_definition",
            definition,
            |kind| match kind {
                GlobalKind::Protocol(slot) => Some(slot),
                _ => None,
            },
        );
    }

    /// Stamp a resolved field layout onto a struct entry. Panics
    /// unless the entry's kind is exactly `Struct(None)`.
    pub(crate) fn set_struct_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: StructDefinition,
    ) {
        self.stamp(id, "set_struct_definition", definition, |kind| match kind {
            GlobalKind::Struct(slot) => Some(slot),
            _ => None,
        });
    }

    fn insert(
        &mut self,
        identifier: Identifier,
        kind: GlobalKind,
        span: Span,
        name_span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        // Kept loose on purpose. This is the one place an entry is
        // built from parts. The public inserts read those parts from
        // the AST node.
        let names = self.by_identifier.entry(identifier.clone()).or_default();
        if let Some(id) = names
            .non_function
            .or_else(|| names.functions.values().next().copied())
        {
            let existing = self
                .entries
                .get(&id)
                .expect("reverse index points at a missing forward entry");
            return InsertOutcome::Collision { existing };
        }
        let id = GlobalRegistryId::new(self.next_id);
        self.next_id += 1;
        names.non_function = Some(id);
        let type_param_bounds = vec![Vec::new(); type_params.len()];
        self.entries.insert(
            id,
            RegistryEntry {
                deprecation: None,
                identifier,
                kind,
                name_span,
                span,
                type_params,
                type_param_bounds,
                visibility,
            },
        );
        InsertOutcome::Fresh(id)
    }

    /// Stamp a resolved type + RHS onto a constant entry. Panics
    /// unless the entry's kind is exactly `Constant(None)`.
    pub(crate) fn set_constant_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: ConstantDefinition,
    ) {
        self.stamp(
            id,
            "set_constant_definition",
            Box::new(definition),
            |kind| match kind {
                GlobalKind::Constant(slot) => Some(slot),
                _ => None,
            },
        );
    }

    /// Stamp a resolved expansion onto a type-alias entry. Panics
    /// unless the entry's kind is exactly `TypeAlias(None)`.
    pub(crate) fn set_type_alias_definition(
        &mut self,
        id: GlobalRegistryId,
        expansion: ResolvedType,
    ) {
        self.stamp(
            id,
            "set_type_alias_definition",
            expansion,
            |kind| match kind {
                GlobalKind::TypeAlias(slot) => Some(slot),
                _ => None,
            },
        );
    }

    /// Look up a registered alias's expansion. `None` if `id` is
    /// not a `TypeAlias` entry, or if it is but the lift pass
    /// has not stamped its expansion yet (mid-lift state).
    /// [`super::pipeline::resolve::types::peel_alias`] uses this to
    /// follow `Named { Global(alias_id) }` to the underlying type.
    pub fn alias_expansion(&self, id: GlobalRegistryId) -> Option<ResolvedType> {
        match self.entries.get(&id)?.kind {
            GlobalKind::TypeAlias(Some(ref expansion)) => Some(expansion.clone()),
            _ => None,
        }
    }

    /// Overwrite an alias's expansion regardless of its current
    /// stamp state. Used by `lift_type_aliases`'s cycle sweep to
    /// rewrite cycling aliases to `ResolvedType::unresolved` so
    /// downstream peels short-circuit cleanly. Panics if `id` is
    /// not a `TypeAlias` entry. Only the cycle pass should call
    /// this.
    pub(crate) fn set_type_alias_definition_force(
        &mut self,
        id: GlobalRegistryId,
        expansion: ResolvedType,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_type_alias_definition_force on missing registry id {id}. This is a \
                 lift invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::TypeAlias(_) => {
                entry.kind = GlobalKind::TypeAlias(Some(expansion));
            }
            other => panic!(
                "set_type_alias_definition_force called on non-alias entry `{}` ({}). \
                 Only TypeAlias entries support force-stamp",
                entry.identifier,
                other.label(),
            ),
        }
    }

    /// Stamp a resolved signature onto a collected function entry.
    pub(crate) fn set_signature(&mut self, id: GlobalRegistryId, signature: FunctionSignature) {
        self.stamp(id, "set_signature", signature, |kind| match kind {
            GlobalKind::Function(definition) => Some(&mut definition.signature),
            _ => None,
        });
    }

    /// Claim a seeded builtin stub for a `builtin` declaration.
    /// Stamps the declaration's spans onto the entry so later
    /// collisions point at real source, and consumes the stub so a
    /// second claim collides like any duplicate. When the declared
    /// type-param arity matches the stub's shape, the entry adopts
    /// the declared names so member lifting resolves against them.
    /// `None` when `identifier` does not name an unclaimed builtin.
    pub(crate) fn claim_builtin_stub(
        &mut self,
        identifier: &Identifier,
        decl: &BuiltinDecl,
    ) -> Option<ClaimOutcome> {
        let id = self.by_identifier.get(identifier)?.non_function?;
        if !self.unclaimed_builtin_stubs.remove(&id) {
            return None;
        }
        let entry = self
            .entries
            .get_mut(&id)
            .expect("reverse index points at a missing forward entry");
        entry.span = decl.span;
        entry.name_span = decl.name().span;
        let GlobalKind::Builtin(definition) = &entry.kind else {
            panic!(
                "unclaimed stub `{}` is not a Builtin entry. This is a seed invariant violation",
                entry.identifier,
            );
        };
        let expected_arity = definition.shape.arity();
        if decl.type_params.len() != expected_arity {
            return Some(ClaimOutcome::ArityMismatch { id, expected_arity });
        }
        entry.type_param_bounds = vec![Vec::new(); decl.type_params.len()];
        entry.type_params = type_param_names(&decl.type_params);
        Some(ClaimOutcome::Claimed(id))
    }

    /// The shape carried by a [`GlobalKind::Builtin`] entry. `None`
    /// for unknown ids and non-builtin entries.
    pub fn builtin_shape(&self, id: GlobalRegistryId) -> Option<BuiltinShape> {
        match &self.get(id)?.kind {
            GlobalKind::Builtin(definition) => Some(definition.shape),
            _ => None,
        }
    }

    /// Dereference an id to its entry.
    pub fn get(&self, id: GlobalRegistryId) -> Option<&RegistryEntry> {
        self.entries.get(&id)
    }

    /// Reverse lookup from an [`Identifier`] to its id + entry. Used by
    /// resolve to stamp ids onto AST reference sites. A name declared
    /// only as functions returns the highest arity, so treat the
    /// result as "some declaration with this name" and use
    /// [`Self::lookup_function`] to select an exact function identity.
    pub fn lookup(&self, identifier: &Identifier) -> Option<(GlobalRegistryId, &RegistryEntry)> {
        let names = self.by_identifier.get(identifier)?;
        let id = names
            .non_function
            .or_else(|| names.functions.values().next_back().copied())?;
        let entry = self.entries.get(&id)?;
        Some((id, entry))
    }

    /// Look up one exact function identity.
    pub fn lookup_function(
        &self,
        identifier: &Identifier,
        arity: usize,
    ) -> Option<(GlobalRegistryId, &RegistryEntry)> {
        let id = *self.by_identifier.get(identifier)?.functions.get(&arity)?;
        Some((id, self.entries.get(&id)?))
    }

    /// Look up a function identity, distinguishing "no function at
    /// this arity" from "no functions under this name at all" so
    /// callers can build did-you-mean diagnostics.
    pub fn function_lookup(&self, identifier: &Identifier, arity: usize) -> FunctionLookup<'_> {
        if let Some(found) = self.lookup_function(identifier, arity) {
            return FunctionLookup::Found(found.0, found.1);
        }
        let arities = self.function_arities(identifier);
        if arities.is_empty() {
            return FunctionLookup::NoFunctions;
        }
        FunctionLookup::WrongArity(arities)
    }

    /// Return the declared arities for every function with this name.
    pub fn function_arities(&self, identifier: &Identifier) -> Vec<usize> {
        self.by_identifier
            .get(identifier)
            .map(|entries| entries.functions.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Resolve a nominal `impl` / `extend` target `path` to its owning
    /// `(id, package, path)`. A same-package nested type (`Outer.Inner`)
    /// wins over the `<package>.<rest>` reading, matching type/value
    /// resolution, and bare stdlib names fall back to `Global`.
    pub fn lookup_owner_path(
        &self,
        path: &[Name],
        current_package: &str,
    ) -> Option<(GlobalRegistryId, String, Vec<String>)> {
        let path = name_texts(path);
        if let Some((id, _)) = self.lookup(&Identifier::new(current_package, path.clone())) {
            return Some((id, current_package.to_string(), path));
        }
        if path.len() >= 2
            && let Some((id, _)) = self.lookup(&Identifier::new(&path[0], path[1..].to_vec()))
        {
            return Some((id, path[0].clone(), path[1..].to_vec()));
        }
        if let Some((id, _)) = self.lookup(&Identifier::new("Global", path.clone())) {
            return Some((id, "Global".to_string(), path));
        }
        None
    }

    /// Build a leaf [`ResolvedType`] pointing at the preloaded
    /// `Global.<name>` stdlib stub. Panics if the stub is missing.
    /// Preload is a [`Self::with_stdlib_stubs`] invariant.
    ///
    /// A cross-pipeline helper. `lift_signatures` calls it when
    /// synthesizing parameter / return types from `TypeExpr::Unit`
    /// and `TypeExpr::Named`, and the resolve pass calls it
    /// (directly and via [`Self::literal_type`]) when stamping
    /// expressions. Both passes want the same panic-on-miss
    /// semantics, so the lookup lives here rather than getting
    /// duplicated per pass.
    pub(crate) fn primitive(&self, name: &str) -> ResolvedType {
        let ident = Identifier::single("Global", name);
        let (id, _) = self.lookup(&ident).unwrap_or_else(|| {
            panic!(
                "stdlib stub `Global.{name}` missing from registry. \
                 pipeline must seed it via `GlobalRegistry::with_stdlib_stubs`",
            )
        });
        ResolvedType::leaf(Resolution::Global(id))
    }

    /// Build the [`ResolvedType`] for a primitive literal. The
    /// `Literal` variants map one-to-one onto preloaded stdlib
    /// stubs (`Bool`, `Float`, `Int`, `String`, `Unit`). Convenience
    /// wrapper over [`Self::primitive`] used by the resolve pass
    /// for `ExprKind::Literal` and pattern-vs-subject coercion, and
    /// by `lift_signatures` when classifying constant initializers.
    /// String *interpolation* (`ExprKind::String`) is a separate,
    /// resolve-only path and stays out of this helper.
    pub(crate) fn literal_type(&self, value: &Literal) -> ResolvedType {
        match value {
            Literal::Bool(_) => self.primitive("Bool"),
            Literal::Float(_) => self.primitive("Float"),
            Literal::Int(_) => self.primitive("Int"),
            Literal::String(_) => self.primitive("String"),
            Literal::Unit => self.primitive("Unit"),
        }
    }

    /// Render the name of a type parameter by its anchored
    /// `(owner, index)`. `None` when `owner` is unknown or `index`
    /// is out of range, which is a compiler bug, since the index should
    /// have come from a [`Resolution::TypeParam`] anchored to the same
    /// owner.
    pub fn type_param_name(&self, owner: GlobalRegistryId, index: TypeParamIndex) -> Option<&str> {
        self.get(owner)?
            .type_params
            .get(index.as_u32() as usize)
            .map(String::as_str)
    }

    /// Slice of generic-decl param names declared on `owner`. `None`
    /// when `owner` is unknown. A known owner with no generics
    /// returns `Some(&[])`. Used by
    /// [`crate::pipeline::lift_signatures::TypeParamScope::lookup`]
    /// to walk a chained scope and turn a name into
    /// `(owner, TypeParamIndex)`.
    pub fn type_params(&self, owner: GlobalRegistryId) -> Option<&[String]> {
        self.get(owner).map(|entry| entry.type_params.as_slice())
    }

    /// Slice of resolved bounds on `owner`'s generic-decl params,
    /// parallel to [`Self::type_params`] (same length, same indexing).
    /// Inner vec is the `&`-composed protocol-bound list for that param.
    /// Empty means unbounded. `None` when `owner` is unknown.
    pub fn type_param_bounds(
        &self,
        owner: GlobalRegistryId,
    ) -> Option<&[Vec<ResolvedProtocolBound>]> {
        self.get(owner)
            .map(|entry| entry.type_param_bounds.as_slice())
    }

    /// Replace `owner`'s `type_param_bounds`. `bounds.len()` must equal
    /// the entry's `type_params.len()`. Called by lift's bounds-resolve
    /// sub-pass after every protocol is registered.
    pub(crate) fn set_type_param_bounds(
        &mut self,
        owner: GlobalRegistryId,
        bounds: Vec<Vec<ResolvedProtocolBound>>,
    ) {
        let entry = self
            .entries
            .get_mut(&owner)
            .unwrap_or_else(|| panic!("set_type_param_bounds on missing registry id {owner}"));
        if bounds.len() != entry.type_params.len() {
            panic!(
                "set_type_param_bounds length mismatch on `{}`. \
                 type_params.len() = {}, bounds.len() = {}",
                entry.identifier,
                entry.type_params.len(),
                bounds.len(),
            );
        }
        entry.type_param_bounds = bounds;
    }

    /// Iterate every entry. `HashMap` iteration is not stable across
    /// runs. Callers needing a deterministic order sort by id (matches
    /// declaration order) or by `entry.identifier.qualified_name()`.
    pub fn iter(&self) -> impl Iterator<Item = (GlobalRegistryId, &RegistryEntry)> {
        self.entries.iter().map(|(id, entry)| (*id, entry))
    }

    /// Iterate every entry whose identifier lives in `pkg`. Same
    /// stability caveat as [`Self::iter`].
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

/// Protocols that every type implicitly satisfies. The synthesizer
/// or hand-written stdlib impls guarantee an impl for every concrete
/// monomorphization, so a bare type-parameter `T.format()` /
/// `T.equals?(other)` resolves as if `T: Debug` / `T: Equality` were
/// declared. `Hash` joins this list once it is auto-derived too.
/// (`Clone` was removed when value semantics made explicit
/// duplication unnecessary, since every value is already independent.)
pub const UNIVERSAL_PROTOCOLS: &[&str] = &["Debug", "Equality"];

/// The declared generic-param names, which is all the registry stores
/// at insert time. Bounds are stamped later by
/// [`GlobalRegistry::set_type_param_bounds`], once every protocol id
/// exists.
fn type_param_names(type_params: &[TypeParam]) -> Vec<String> {
    type_params.iter().map(|p| p.name.text.clone()).collect()
}

/// Seed a builtin stub under `Global.<name>` carrying `shape` and an
/// empty conformance map.
fn seed_builtin_stub(
    reg: &mut GlobalRegistry,
    name: &str,
    shape: BuiltinShape,
    type_params: Vec<String>,
) {
    let kind = GlobalKind::Builtin(BuiltinDefinition {
        conformances: BTreeMap::new(),
        shape,
    });
    let outcome = reg.insert(
        Identifier::single("Global", name),
        kind,
        Span::default(),
        Span::default(),
        type_params,
        VisibilityScope::Public,
    );
    let id = match outcome {
        InsertOutcome::Fresh(id) => id,
        InsertOutcome::Collision { existing } => panic!(
            "stdlib stub `Global.{name}` collided on preload with `{}`. \
             registry was not empty",
            existing.identifier,
        ),
    };
    reg.unclaimed_builtin_stubs.insert(id);
}

#[cfg(test)]
mod tests {
    use koja_ast::ast::{Name, Visibility};
    use koja_ast::span::{FileId, Position};

    use super::*;

    fn span_on_line_3(start: u32, end: u32) -> Span {
        let position = |column| Position {
            offset: column,
            line: 3,
            column,
        };
        Span::new(position(start), position(end), FileId::UNKNOWN)
    }

    fn decl_span() -> Span {
        span_on_line_3(1, 20)
    }

    fn name_span() -> Span {
        span_on_line_3(9, 15)
    }

    /// A `builtin <name><params>` declaration at [`decl_span`] with
    /// its name at [`name_span`].
    fn builtin_decl(name: &str, params: &[&str]) -> BuiltinDecl {
        BuiltinDecl {
            annotations: Vec::new(),
            visibility: Visibility::Public,
            path: vec![Name::new(name, name_span())],
            type_params: params
                .iter()
                .map(|param| TypeParam {
                    name: Name::new(*param, name_span()),
                    bounds: Vec::new(),
                    span: name_span(),
                })
                .collect(),
            functions: Vec::new(),
            nested: Vec::new(),
            span: decl_span(),
            tests: Vec::new(),
        }
    }

    #[test]
    fn claim_builtin_stub_stamps_spans_and_consumes_stub() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::single("Global", "String");
        let decl = builtin_decl("String", &[]);

        let Some(ClaimOutcome::Claimed(id)) = reg.claim_builtin_stub(&identifier, &decl) else {
            panic!("seeded `Global.String` stub should be claimable");
        };
        assert_eq!(reg.get(id).unwrap().span, decl_span());
        assert_eq!(reg.get(id).unwrap().name_span, name_span());

        assert!(
            reg.claim_builtin_stub(&identifier, &decl).is_none(),
            "a stub claims at most once",
        );
    }

    #[test]
    fn claim_builtin_stub_adopts_declared_param_names() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::single("Global", "List");
        let decl = builtin_decl("List", &["Elem"]);

        let Some(ClaimOutcome::Claimed(id)) = reg.claim_builtin_stub(&identifier, &decl) else {
            panic!("seeded `Global.List` stub should be claimable");
        };
        assert_eq!(reg.type_params(id), Some(&["Elem".to_string()][..]));
    }

    #[test]
    fn claim_builtin_stub_reports_arity_mismatch() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::single("Global", "Map");
        let decl = builtin_decl("Map", &["K"]);

        let Some(ClaimOutcome::ArityMismatch { id, expected_arity }) =
            reg.claim_builtin_stub(&identifier, &decl)
        else {
            panic!("wrong arity should report a mismatch");
        };
        assert_eq!(expected_arity, 2);
        assert_eq!(
            reg.type_params(id),
            Some(&["K".to_string(), "V".to_string()][..]),
            "mismatched claim keeps the seeded param names",
        );
    }

    #[test]
    fn claim_builtin_stub_rejects_non_builtin_identifiers() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let user_type = Identifier::single("App", "Config");
        let decl = builtin_decl("Config", &[]);
        let InsertOutcome::Fresh(_) =
            reg.insert_unclaimed_builtin(user_type.clone(), &decl, VisibilityScope::Public)
        else {
            panic!("fresh registry should accept `App.Config`");
        };

        assert!(reg.claim_builtin_stub(&user_type, &decl).is_none());
        let missing = Identifier::single("App", "Missing");
        assert!(
            reg.claim_builtin_stub(&missing, &builtin_decl("Missing", &[]))
                .is_none()
        );
    }
}
