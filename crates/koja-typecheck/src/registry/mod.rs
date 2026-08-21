//! Global registry of every uniquely-named declaration, keyed by
//! [`GlobalRegistryId`] and reverse-indexed by [`Identifier`]. The
//! registry is the authoritative gate enforcing identifier uniqueness.
//! Insert sites emit the "already defined" diagnostic when an insert
//! returns [`InsertOutcome::Collision`].
//!
//! Today only top-level structs, enums, functions, and protocols
//! register. Methods, enum variants, constants, and type aliases land
//! as the surrounding pipeline migrates onto path-based
//! [`Identifier`]s.
//!
//! Ids are assigned sequentially (monotonic `u32` counter). A future
//! parallel-cache story will swap in content-addressable hashing
//! without changing the public surface.
//!
//! # Function signatures
//!
//! [`GlobalKind::Function`] carries its signature inline as
//! `Option<FunctionSignature>`: `None` is the "collected but not yet
//! lifted" state, `Some(sig)` the "lifted" state reached after
//! `lift_signatures` runs. The variant-carried design makes illegal
//! states unrepresentable: non-function entries literally cannot
//! carry a signature.
//!
//! Registry rendering for `koja check --emit-ast` lives in the
//! [`format`] submodule. It's a separate concern from the data + insert
//! API (different audience: diagnostic rendering vs pipeline work).

use std::collections::{BTreeMap, HashMap, HashSet};

use koja_ast::ast::Literal;
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

mod candidates;
mod definitions;
mod format;

pub use candidates::{Candidate, CandidateDetail, CandidateKind, KEYWORDS};
pub use definitions::{
    BoundOverlay, BuiltinDefinition, BuiltinShape, Conformance, ConformanceScope,
    ConstantDefinition, Dispatch, EnumDefinition, FunctionSignature, ProtocolDefinition,
    ResolvedEnumVariant, ResolvedParam, ResolvedProtocolMethod, ResolvedStructField,
    ResolvedVariantData, StructDefinition,
};
pub use format::format_registry;

use crate::pipeline::resolve::types::types_equivalent;

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
    /// No `Option` lifecycle: the shape is stamped at seed time.
    Builtin(BuiltinDefinition),
    Constant(Option<Box<ConstantDefinition>>),
    Enum(Option<EnumDefinition>),
    Function(Option<FunctionSignature>),
    Protocol(Option<ProtocolDefinition>),
    Struct(Option<StructDefinition>),
    /// `type X = ...` declared at top level. The `Option` mirrors
    /// other lifecycle-payload variants: `None` after collect,
    /// `Some(expansion)` after `lift_type_aliases` resolves the RHS.
    /// The expansion is the canonical [`ResolvedType`] the alias
    /// stands for. For the surface-aliasing case
    /// (`type Pet = Cat | Dog | Fish`) that's typically a
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

/// A single registered declaration: canonical [`Identifier`],
/// [`GlobalKind`], source span (used for "already defined here"
/// diagnostic notes), and any generic-decl param names declared on
/// it. `type_params` is stamped at collect time directly from the
/// AST so [`GlobalRegistry::type_params`] is queryable mid-lift,
/// before [`StructDefinition`] / [`EnumDefinition`] / signature
/// payloads are stamped.
///
/// `type_param_bounds` is parallel to `type_params` (same length, same
/// indexing). Each inner `Vec<GlobalRegistryId>` holds the protocol ids
/// from a `<T: P1 & P2>` bound, in source order. Empty inner vec means
/// the param is unbounded. Default at collect time is one empty inner
/// vec per param. Lift's bounds-resolve sub-pass replaces it with the
/// resolved protocol ids via [`GlobalRegistry::set_type_param_bounds`].
///
/// `visibility` carries the `priv` enforcement scope as a
/// [`VisibilityScope`]. See that enum for the three-case rationale.
/// Functions can be `TypePrivate`. Every other entry kind is either
/// `Public` or `PackagePrivate`.
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    /// `@deprecated` message, always non-empty. `None` means not
    /// deprecated.
    pub deprecation: Option<String>,
    pub identifier: Identifier,
    pub kind: GlobalKind,
    pub span: Span,
    pub type_params: Vec<String>,
    pub type_param_bounds: Vec<Vec<GlobalRegistryId>>,
    pub visibility: VisibilityScope,
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
///   [`Identifier`] so it doesn't need to be repeated here.
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
    by_identifier: HashMap<Identifier, GlobalRegistryId>,
    next_id: u32,
    /// Seeded builtin stubs not yet claimed by a `builtin`
    /// declaration. [`Self::claim_builtin_stub`] drains it.
    unclaimed_builtin_stubs: HashSet<GlobalRegistryId>,
}

impl GlobalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a fresh registry with one [`GlobalKind::Builtin`] stub
    /// per compiler-owned type, all under the `Global` package so
    /// resolve never special-cases them. `Option<T>` is *not*
    /// stubbed: it's an ordinary enum in autoimported
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
    /// later by [`Self::set_constant_definition`]. Constants don't
    /// take type parameters, so callers always pass an empty vec.
    pub(crate) fn insert_constant(
        &mut self,
        identifier: Identifier,
        span: Span,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Constant(None),
            span,
            Vec::new(),
            visibility,
        )
    }

    /// Register an enum in the `Enum(None)` state. The resolved
    /// variant roster is stamped in later by
    /// [`Self::set_enum_definition`]. `type_params` carries the
    /// declared generic-param names from the AST so resolve and
    /// lift can answer "what params are in scope inside this decl?"
    /// before the variant payload types have been resolved.
    pub(crate) fn insert_enum(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Enum(None),
            span,
            type_params,
            visibility,
        )
    }

    /// Register a function in the `Function(None)` state. The
    /// signature is stamped in later by [`Self::set_signature`].
    /// `type_params` carries the function's own declared generic
    /// params (not the enclosing struct/impl's). Chained scopes are
    /// rebuilt at resolve time.
    ///
    /// `visibility` captures the `priv fn` enforcement scope as a
    /// [`VisibilityScope`]: `Public` for default `fn`, or the
    /// `PackagePrivate` / `TypePrivate(type_id)` variant that
    /// matches where the `priv fn` was declared. See
    /// [`VisibilityScope`] for the mapping rule.
    pub(crate) fn insert_function(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Function(None),
            span,
            type_params,
            visibility,
        )
    }

    /// Register a protocol in the `Protocol(None)` state. Method
    /// roster is stamped later by [`Self::set_protocol_definition`].
    pub(crate) fn insert_protocol(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Protocol(None),
            span,
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
        span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::Struct(None),
            span,
            type_params,
            visibility,
        )
    }

    /// Register a `type X = ...` alias in the `TypeAlias(None)`
    /// state. The expansion is stamped in later by
    /// [`Self::set_type_alias_definition`]. Aliases don't take
    /// generic params today, so callers always pass an empty vec.
    /// Generic aliases are a possible future language extension.
    pub(crate) fn insert_type_alias(
        &mut self,
        identifier: Identifier,
        span: Span,
        visibility: VisibilityScope,
    ) -> InsertOutcome<'_> {
        self.insert(
            identifier,
            GlobalKind::TypeAlias(None),
            span,
            Vec::new(),
            visibility,
        )
    }

    /// Stamp a `@deprecated` message onto an entry. Collect calls
    /// this at most once per decl, right after a fresh insert.
    pub(crate) fn set_deprecation(&mut self, id: GlobalRegistryId, message: String) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_deprecation on missing registry id {id}: collect invariant violation")
        });
        entry.deprecation = Some(message);
    }

    /// Stamp a resolved variant roster onto an enum entry. Panics
    /// unless the entry's kind is exactly `Enum(None)`.
    pub(crate) fn set_enum_definition(&mut self, id: GlobalRegistryId, definition: EnumDefinition) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_enum_definition on missing registry id {id}: collect invariant violation")
        });
        match &entry.kind {
            GlobalKind::Enum(None) => {
                entry.kind = GlobalKind::Enum(Some(definition));
            }
            GlobalKind::Enum(Some(_)) => {
                panic!(
                    "set_enum_definition called twice on `{}`: lift_signatures must stamp \
                     each enum exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_enum_definition called on non-enum entry `{}` ({}): \
                     only Enum entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Record a [`Conformance`] of `target_id` to `protocol_id`.
    /// Returns the previously-recorded record when the new one
    /// overlaps it (a `Parameterized` record overlaps everything,
    /// two `Concrete` records overlap when their target args are
    /// equivalent). The caller emits the "duplicate `impl P for T`"
    /// diagnostic. Panics unless `target_id` names a builtin or a
    /// struct/enum with a stamped definition (lift orders
    /// enum/struct definition stamping before impl conformance
    /// recording).
    pub(crate) fn record_conformance(
        &mut self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        conformance: Conformance,
    ) -> Option<Conformance> {
        // Overlap check first, since type equivalence needs `&self`
        // while the insert below holds the entry mutably.
        if let Some(existing) = self
            .conformance_records(target_id, protocol_id)
            .unwrap_or_default()
            .iter()
            .find(|record| self.conformances_overlap(record, &conformance))
        {
            return Some(existing.clone());
        }
        let entry = self.entries.get_mut(&target_id).unwrap_or_else(|| {
            panic!(
                "record_conformance on missing registry id {target_id}: \
                 lift invariant violation",
            )
        });
        let conformances = match &mut entry.kind {
            GlobalKind::Builtin(def) => &mut def.conformances,
            GlobalKind::Struct(Some(def)) => &mut def.conformances,
            GlobalKind::Enum(Some(def)) => &mut def.conformances,
            other => panic!(
                "record_conformance on `{}` ({}): only builtin and stamped \
                 struct/enum entries accept conformances",
                entry.identifier,
                other.label(),
            ),
        };
        conformances
            .entry(protocol_id)
            .or_default()
            .push(conformance);
        None
    }

    /// The conformance record of `target_id` to `protocol_id` that
    /// covers the `target_args` instantiation. `Concrete` covers
    /// exactly its recorded args, `Parameterized` covers every
    /// instantiation whose args discharge the record's conditional
    /// bounds. Typecheck uses this for bound enforcement and
    /// `spawn`. IR's bounded dispatch never reaches this path (it
    /// goes straight to `[target, method_name]`).
    pub fn lookup_conformance(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
    ) -> Option<&Conformance> {
        self.lookup_conformance_with(target_id, protocol_id, target_args, None)
    }

    /// Like [`Self::lookup_conformance`], with an impl-local
    /// [`BoundOverlay`] so obligations raised inside a conditional
    /// impl body can discharge through the impl's own condition.
    pub fn lookup_conformance_with(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
    ) -> Option<&Conformance> {
        self.conformance_records(target_id, protocol_id)?
            .iter()
            .find(|record| match &record.scope {
                ConformanceScope::Concrete(args) => self.type_args_equivalent(args, target_args),
                ConformanceScope::Parameterized { bounds } => {
                    self.conformance_bounds_satisfied(bounds, target_args, overlay)
                }
            })
    }

    /// Whether every target arg discharges its slot's conditional
    /// bounds. Unconditional records carry empty `bounds`, and a
    /// lookup with no args (head-level consumers) has nothing to
    /// check, so both zip to vacuous truth.
    fn conformance_bounds_satisfied(
        &self,
        bounds: &[Vec<GlobalRegistryId>],
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        bounds.iter().zip(target_args).all(|(slot_bounds, arg)| {
            slot_bounds
                .iter()
                .all(|&protocol_id| self.bound_satisfied(arg, protocol_id, overlay))
        })
    }

    /// Whether `ty` discharges a `ty: protocol` obligation. Named
    /// types consult their conformance records recursively (so
    /// `List<List<Int>>: Equality` walks down). Type params
    /// discharge through universal protocols, their declared
    /// bounds, or the overlay. Tuples are structurally `Debug`,
    /// and structurally `Equality` when every element is. Other
    /// shapes (functions, unresolved) satisfy nothing.
    pub fn bound_satisfied(
        &self,
        ty: &ResolvedType,
        protocol_id: GlobalRegistryId,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        match ty {
            ResolvedType::Named {
                resolution: Resolution::Global(target_id),
                type_args,
            } => self
                .lookup_conformance_with(*target_id, protocol_id, type_args, overlay)
                .is_some(),
            ResolvedType::Named {
                resolution: Resolution::TypeParam { owner, index },
                ..
            } => self.type_param_bound_granted(*owner, *index, protocol_id, overlay),
            ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
                self.tuple_bound_satisfied(elements, protocol_id, overlay)
            }
            _ => false,
        }
    }

    /// A type param discharges a bound through a universal
    /// protocol, its declared bounds, or an overlay slot.
    fn type_param_bound_granted(
        &self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
        protocol_id: GlobalRegistryId,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        if self.is_universal_protocol(protocol_id) {
            return true;
        }
        let slot = index.as_u32() as usize;
        let declared = self
            .type_param_bounds(owner)
            .and_then(|all| all.get(slot))
            .is_some_and(|bounds| bounds.contains(&protocol_id));
        declared
            || overlay.is_some_and(|o| {
                o.owner == owner
                    && o.bounds
                        .get(slot)
                        .is_some_and(|bounds| bounds.contains(&protocol_id))
            })
    }

    /// Structural tuple conformance: `Debug` unconditionally
    /// (opaque elements render as `"..."`), `Equality` when every
    /// element satisfies it, nothing else.
    fn tuple_bound_satisfied(
        &self,
        elements: &[ResolvedType],
        protocol_id: GlobalRegistryId,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        let Some(entry) = self.get(protocol_id) else {
            return false;
        };
        if entry.identifier.package() != "Global" || entry.identifier.path().len() != 1 {
            return false;
        }
        match entry.identifier.last() {
            "Debug" => true,
            "Equality" => elements
                .iter()
                .all(|element| self.bound_satisfied(element, protocol_id, overlay)),
            _ => false,
        }
    }

    /// Whether `protocol_id` names a universal protocol
    /// ([`UNIVERSAL_PROTOCOLS`]), which every type param satisfies
    /// without a declared bound.
    pub fn is_universal_protocol(&self, protocol_id: GlobalRegistryId) -> bool {
        self.get(protocol_id).is_some_and(|entry| {
            entry.identifier.package() == "Global"
                && entry.identifier.path().len() == 1
                && UNIVERSAL_PROTOCOLS.contains(&entry.identifier.last())
        })
    }

    /// Whether `target_id` conforms to `protocol_id` under any
    /// instantiation. For consumers that only ask "which protocols"
    /// and have no instantiation at hand (monitor, carriers, the
    /// driver's Task check).
    pub fn conforms_any(&self, target_id: GlobalRegistryId, protocol_id: GlobalRegistryId) -> bool {
        self.conformance_records(target_id, protocol_id)
            .is_some_and(|records| !records.is_empty())
    }

    /// Every recorded conformance of `target_id` to `protocol_id`,
    /// or `None` when the entry is not a builtin/struct/enum or has
    /// no record for that protocol.
    pub fn conformance_records(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
    ) -> Option<&[Conformance]> {
        let entry = self.entries.get(&target_id)?;
        let conformances = match &entry.kind {
            GlobalKind::Builtin(def) => &def.conformances,
            GlobalKind::Struct(Some(def)) => &def.conformances,
            GlobalKind::Enum(Some(def)) => &def.conformances,
            _ => return None,
        };
        conformances.get(&protocol_id).map(Vec::as_slice)
    }

    /// Whether two records for one `(target, protocol)` pair claim
    /// an overlapping set of instantiations.
    fn conformances_overlap(&self, a: &Conformance, b: &Conformance) -> bool {
        match (&a.scope, &b.scope) {
            (ConformanceScope::Parameterized { .. }, _)
            | (_, ConformanceScope::Parameterized { .. }) => true,
            (ConformanceScope::Concrete(a_args), ConformanceScope::Concrete(b_args)) => {
                self.type_args_equivalent(a_args, b_args)
            }
        }
    }

    /// Pairwise [`types_equivalent`] over two arg lists.
    fn type_args_equivalent(&self, a: &[ResolvedType], b: &[ResolvedType]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| types_equivalent(x, y, self))
    }

    /// Stamp a resolved method roster. Panics unless the entry's
    /// kind is exactly `Protocol(None)`.
    pub(crate) fn set_protocol_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: ProtocolDefinition,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_protocol_definition on missing registry id {id}: collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::Protocol(None) => {
                entry.kind = GlobalKind::Protocol(Some(definition));
            }
            GlobalKind::Protocol(Some(_)) => {
                panic!(
                    "set_protocol_definition called twice on `{}`: lift_signatures must stamp \
                     each protocol exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_protocol_definition called on non-protocol entry `{}` ({}): \
                     only Protocol entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Stamp a resolved field layout onto a struct entry. Panics
    /// unless the entry's kind is exactly `Struct(None)`.
    pub(crate) fn set_struct_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: StructDefinition,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_struct_definition on missing registry id {id}: collect invariant violation")
        });
        match &entry.kind {
            GlobalKind::Struct(None) => {
                entry.kind = GlobalKind::Struct(Some(definition));
            }
            GlobalKind::Struct(Some(_)) => {
                panic!(
                    "set_struct_definition called twice on `{}`: lift_signatures must stamp \
                     each struct exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_struct_definition called on non-struct entry `{}` ({}): \
                     only Struct entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    fn insert(
        &mut self,
        identifier: Identifier,
        kind: GlobalKind,
        span: Span,
        type_params: Vec<String>,
        visibility: VisibilityScope,
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
        let type_param_bounds = vec![Vec::new(); type_params.len()];
        self.entries.insert(
            id,
            RegistryEntry {
                deprecation: None,
                identifier,
                kind,
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
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_constant_definition on missing registry id {id}: collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::Constant(None) => {
                entry.kind = GlobalKind::Constant(Some(Box::new(definition)));
            }
            GlobalKind::Constant(Some(_)) => {
                panic!(
                    "set_constant_definition called twice on `{}`: lift_signatures must stamp \
                     each constant exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_constant_definition called on non-constant entry `{}` ({}): \
                     only Constant entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Stamp a resolved expansion onto a type-alias entry. Panics
    /// unless the entry's kind is exactly `TypeAlias(None)`.
    pub(crate) fn set_type_alias_definition(
        &mut self,
        id: GlobalRegistryId,
        expansion: ResolvedType,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_type_alias_definition on missing registry id {id}: \
                 collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::TypeAlias(None) => {
                entry.kind = GlobalKind::TypeAlias(Some(expansion));
            }
            GlobalKind::TypeAlias(Some(_)) => {
                panic!(
                    "set_type_alias_definition called twice on `{}`: \
                     lift_type_aliases must stamp each alias exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_type_alias_definition called on non-alias entry `{}` ({}): \
                     only TypeAlias entries carry expansions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Look up a registered alias's expansion. `None` if `id` is
    /// not a `TypeAlias` entry, or if it is but the lift pass
    /// hasn't stamped its expansion yet (mid-lift state).
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
                "set_type_alias_definition_force on missing registry id {id}: \
                 lift invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::TypeAlias(_) => {
                entry.kind = GlobalKind::TypeAlias(Some(expansion));
            }
            other => panic!(
                "set_type_alias_definition_force called on non-alias entry `{}` ({}): \
                 only TypeAlias entries support force-stamp",
                entry.identifier,
                other.label(),
            ),
        }
    }

    /// Stamp a resolved signature onto a function entry. Panics unless
    /// the entry's kind is exactly `Function(None)`.
    pub(crate) fn set_signature(&mut self, id: GlobalRegistryId, signature: FunctionSignature) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_signature on missing registry id {id}: collect invariant violation")
        });
        match &entry.kind {
            GlobalKind::Function(None) => {
                entry.kind = GlobalKind::Function(Some(signature));
            }
            GlobalKind::Function(Some(_)) => {
                panic!(
                    "set_signature called twice on `{}`: lift_signatures must stamp each \
                     function exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_signature called on non-function entry `{}` ({}): \
                     only Function entries carry signatures",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Claim a seeded builtin stub for a `builtin` declaration.
    /// Stamps the declaration's span onto the entry so later
    /// collisions point at real source, and consumes the stub so a
    /// second claim collides like any duplicate. When the declared
    /// type-param arity matches the stub's shape, the entry adopts
    /// the declared names so member lifting resolves against them.
    /// `None` when `identifier` doesn't name an unclaimed builtin.
    pub(crate) fn claim_builtin_stub(
        &mut self,
        identifier: &Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> Option<ClaimOutcome> {
        let id = *self.by_identifier.get(identifier)?;
        if !self.unclaimed_builtin_stubs.remove(&id) {
            return None;
        }
        let entry = self
            .entries
            .get_mut(&id)
            .expect("reverse index points at a missing forward entry");
        entry.span = span;
        let GlobalKind::Builtin(definition) = &entry.kind else {
            panic!(
                "unclaimed stub `{}` is not a Builtin entry: seed invariant violation",
                entry.identifier,
            );
        };
        let expected_arity = definition.shape.arity();
        if type_params.len() != expected_arity {
            return Some(ClaimOutcome::ArityMismatch { id, expected_arity });
        }
        entry.type_param_bounds = vec![Vec::new(); type_params.len()];
        entry.type_params = type_params;
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

    /// Reverse lookup: an [`Identifier`] to its id + entry. Used by
    /// resolve to stamp ids onto AST reference sites.
    pub fn lookup(&self, identifier: &Identifier) -> Option<(GlobalRegistryId, &RegistryEntry)> {
        let id = *self.by_identifier.get(identifier)?;
        let entry = self.entries.get(&id)?;
        Some((id, entry))
    }

    /// Resolve a nominal `impl` / `extend` target `path` to its owning
    /// `(id, package, path)`. A same-package nested type (`Outer.Inner`)
    /// wins over the `<package>.<rest>` reading, matching type/value
    /// resolution, and bare stdlib names fall back to `Global`.
    pub fn lookup_owner_path(
        &self,
        path: &[String],
        current_package: &str,
    ) -> Option<(GlobalRegistryId, String, Vec<String>)> {
        if let Some((id, _)) = self.lookup(&Identifier::new(current_package, path.to_vec())) {
            return Some((id, current_package.to_string(), path.to_vec()));
        }
        if path.len() >= 2
            && let Some((id, _)) = self.lookup(&Identifier::new(&path[0], path[1..].to_vec()))
        {
            return Some((id, path[0].clone(), path[1..].to_vec()));
        }
        if let Some((id, _)) = self.lookup(&Identifier::new("Global", path.to_vec())) {
            return Some((id, "Global".to_string(), path.to_vec()));
        }
        None
    }

    /// Build a leaf [`ResolvedType`] pointing at the preloaded
    /// `Global.<name>` stdlib stub. Panics if the stub is missing.
    /// Preload is a [`Self::with_stdlib_stubs`] invariant.
    ///
    /// Cross-pipeline helper: `lift_signatures` calls it when
    /// synthesizing parameter / return types from `TypeExpr::Unit`
    /// and `TypeExpr::Named`, and the resolve pass calls it
    /// (directly and via [`Self::literal_type`]) when stamping
    /// expressions. Both passes want the same panic-on-miss
    /// semantics, so the lookup lives here rather than getting
    /// duplicated per pass.
    pub(crate) fn primitive(&self, name: &str) -> ResolvedType {
        let ident = Identifier::new("Global", vec![name.to_string()]);
        let (id, _) = self.lookup(&ident).unwrap_or_else(|| {
            panic!(
                "stdlib stub `Global.{name}` missing from registry: \
                 pipeline must seed it via `GlobalRegistry::with_stdlib_stubs`",
            )
        });
        ResolvedType::leaf(Resolution::Global(id))
    }

    /// Build the [`ResolvedType`] for a primitive literal: the
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
    /// is out of range (compiler bug: index should have come from
    /// a [`Resolution::TypeParam`] anchored to the same owner).
    pub fn type_param_name(&self, owner: GlobalRegistryId, index: TypeParamIndex) -> Option<&str> {
        self.get(owner)?
            .type_params
            .get(index.as_u32() as usize)
            .map(String::as_str)
    }

    /// Slice of generic-decl param names declared on `owner`. `None`
    /// when `owner` is unknown. A known owner with no generics
    /// returns `Some(&[])`. Used by
    /// [`crate::pipeline::lift_signatures::types::TypeParamScope::lookup`]
    /// to walk a chained scope and turn a name into
    /// `(owner, TypeParamIndex)`.
    pub fn type_params(&self, owner: GlobalRegistryId) -> Option<&[String]> {
        self.get(owner).map(|entry| entry.type_params.as_slice())
    }

    /// Slice of resolved bounds on `owner`'s generic-decl params,
    /// parallel to [`Self::type_params`] (same length, same indexing).
    /// Inner vec is the `&`-composed protocol-id list for that param.
    /// Empty means unbounded. `None` when `owner` is unknown.
    pub fn type_param_bounds(&self, owner: GlobalRegistryId) -> Option<&[Vec<GlobalRegistryId>]> {
        self.get(owner)
            .map(|entry| entry.type_param_bounds.as_slice())
    }

    /// Replace `owner`'s `type_param_bounds`. `bounds.len()` must equal
    /// the entry's `type_params.len()`. Called by lift's bounds-resolve
    /// sub-pass after every protocol id is registered.
    pub(crate) fn set_type_param_bounds(
        &mut self,
        owner: GlobalRegistryId,
        bounds: Vec<Vec<GlobalRegistryId>>,
    ) {
        let entry = self
            .entries
            .get_mut(&owner)
            .unwrap_or_else(|| panic!("set_type_param_bounds on missing registry id {owner}"));
        if bounds.len() != entry.type_params.len() {
            panic!(
                "set_type_param_bounds length mismatch on `{}`: \
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

    /// Resolve [`UNIVERSAL_PROTOCOLS`] to their `GlobalRegistryId`s.
    /// A name that isn't registered yet (e.g. before `Global.debug`
    /// has been collected) is silently skipped. Callers should only
    /// observe a non-empty list once the stdlib has loaded. Order
    /// follows the source-order of [`UNIVERSAL_PROTOCOLS`].
    pub fn universal_protocol_ids(&self) -> Vec<GlobalRegistryId> {
        UNIVERSAL_PROTOCOLS
            .iter()
            .filter_map(|name| {
                let identifier = Identifier::new("Global", vec![(*name).to_string()]);
                self.lookup(&identifier).map(|(id, _)| id)
            })
            .collect()
    }
}

/// Protocols that every type implicitly satisfies: the synthesizer
/// or hand-written stdlib impls guarantee an impl for every concrete
/// monomorphization, so a bare type-parameter `T.format()` /
/// `T.eq(other)` resolves as if `T: Debug` / `T: Equality` were
/// declared. `Hash` joins this list once it's auto-derived too.
/// (`Clone` was removed when value semantics made explicit
/// duplication unnecessary: every value is already independent.)
pub const UNIVERSAL_PROTOCOLS: &[&str] = &["Debug", "Equality"];

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
        Identifier::new("Global", vec![name.to_string()]),
        kind,
        Span::default(),
        type_params,
        VisibilityScope::Public,
    );
    let id = match outcome {
        InsertOutcome::Fresh(id) => id,
        InsertOutcome::Collision { existing } => panic!(
            "stdlib stub `Global.{name}` collided on preload with `{}`: \
             registry was not empty",
            existing.identifier,
        ),
    };
    reg.unclaimed_builtin_stubs.insert(id);
}

#[cfg(test)]
mod tests {
    use koja_ast::span::{FileId, Position};

    use super::*;

    fn decl_span() -> Span {
        let position = |column| Position {
            offset: column,
            line: 3,
            column,
        };
        Span::new(position(1), position(20), FileId::UNKNOWN)
    }

    #[test]
    fn claim_builtin_stub_stamps_span_and_consumes_stub() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::new("Global", vec!["String".to_string()]);

        let Some(ClaimOutcome::Claimed(id)) =
            reg.claim_builtin_stub(&identifier, decl_span(), Vec::new())
        else {
            panic!("seeded `Global.String` stub should be claimable");
        };
        assert_eq!(reg.get(id).unwrap().span, decl_span());

        assert!(
            reg.claim_builtin_stub(&identifier, Span::default(), Vec::new())
                .is_none(),
            "a stub claims at most once",
        );
    }

    #[test]
    fn claim_builtin_stub_adopts_declared_param_names() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::new("Global", vec!["List".to_string()]);

        let Some(ClaimOutcome::Claimed(id)) =
            reg.claim_builtin_stub(&identifier, decl_span(), vec!["Elem".to_string()])
        else {
            panic!("seeded `Global.List` stub should be claimable");
        };
        assert_eq!(reg.type_params(id), Some(&["Elem".to_string()][..]));
    }

    #[test]
    fn claim_builtin_stub_reports_arity_mismatch() {
        let mut reg = GlobalRegistry::with_stdlib_stubs();
        let identifier = Identifier::new("Global", vec!["Map".to_string()]);

        let Some(ClaimOutcome::ArityMismatch { id, expected_arity }) =
            reg.claim_builtin_stub(&identifier, decl_span(), vec!["K".to_string()])
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
        let user_struct = Identifier::new("App", vec!["Config".to_string()]);
        let InsertOutcome::Fresh(_) = reg.insert_struct(
            user_struct.clone(),
            Span::default(),
            Vec::new(),
            VisibilityScope::Public,
        ) else {
            panic!("fresh registry should accept `App.Config`");
        };

        assert!(
            reg.claim_builtin_stub(&user_struct, decl_span(), Vec::new())
                .is_none()
        );
        let missing = Identifier::new("App", vec!["Missing".to_string()]);
        assert!(
            reg.claim_builtin_stub(&missing, decl_span(), Vec::new())
                .is_none()
        );
    }
}
