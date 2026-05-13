//! Global registry of every uniquely-named declaration, keyed by
//! [`GlobalRegistryId`] and reverse-indexed by [`Identifier`]. The
//! registry is the authoritative gate enforcing identifier uniqueness;
//! insert sites emit the "already defined" diagnostic when an insert
//! returns [`InsertOutcome::Collision`].
//!
//! Today only top-level structs, enums, functions, and protocols
//! register. Methods, enum variants, constants, and type aliases land
//! as the surrounding pipeline migrates onto path-based
//! [`Identifier`]s.
//!
//! Ids are assigned sequentially (monotonic `u32` counter); a future
//! parallel-cache story will swap in content-addressable hashing
//! without changing the public surface.
//!
//! # Function signatures
//!
//! [`GlobalKind::Function`] carries its signature inline as
//! `Option<FunctionSignature>`: `None` is the "collected but not yet
//! lifted" state, `Some(sig)` the "lifted" state reached after
//! `lift_signatures` runs. The variant-carried design makes illegal
//! states unrepresentable — non-function entries literally cannot
//! carry a signature.
//!
//! Registry rendering for `expo alpha check --emit-ast` lives in the
//! [`format`] submodule; it's a separate concern from the data + insert
//! API (different audience: diagnostic rendering vs pipeline work).

use std::collections::{BTreeMap, HashMap};

use expo_ast::ast::Literal;
use expo_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::span::Span;

mod definitions;
mod format;

pub use definitions::{
    ConstantDefinition, Dispatch, EnumDefinition, FunctionSignature, ProtocolDefinition,
    ResolvedEnumVariant, ResolvedParam, ResolvedProtocolMethod, ResolvedStructField,
    ResolvedVariantData, StructDefinition,
};
pub use format::format_registry;

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
/// receiver entry self-contained for IR — see
/// [`StructDefinition::conformances`] for the full rationale.
#[derive(Clone, Debug)]
pub enum GlobalKind {
    Constant(Option<Box<ConstantDefinition>>),
    Enum(Option<EnumDefinition>),
    Function(Option<FunctionSignature>),
    Protocol(Option<ProtocolDefinition>),
    Struct(Option<StructDefinition>),
    /// `type X = ...` declared at top level. The `Option` mirrors
    /// other lifecycle-payload variants: `None` after collect,
    /// `Some(expansion)` after `lift_type_aliases` resolves the RHS.
    /// The expansion is the canonical [`ResolvedType`] the alias
    /// stands for; for the surface-aliasing case
    /// (`type Pet = Cat | Dog | Fish`) that's typically a
    /// canonical [`ResolvedType::Union`], but any `ResolvedType`
    /// shape is permissible.
    TypeAlias(Option<ResolvedType>),
}

impl GlobalKind {
    pub fn label(&self) -> &'static str {
        match self {
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
/// AST so [`GlobalRegistry::type_params`] is queryable mid-lift —
/// before [`StructDefinition`] / [`EnumDefinition`] / signature
/// payloads are stamped.
///
/// `type_param_bounds` is parallel to `type_params` (same length, same
/// indexing). Each inner `Vec<GlobalRegistryId>` holds the protocol ids
/// from a `<T: P1 & P2>` bound, in source order. Empty inner vec means
/// the param is unbounded. Default at collect time is one empty inner
/// vec per param; lift's bounds-resolve sub-pass replaces it with the
/// resolved protocol ids via [`GlobalRegistry::set_type_param_bounds`].
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub identifier: Identifier,
    pub kind: GlobalKind,
    pub span: Span,
    pub type_params: Vec<String>,
    pub type_param_bounds: Vec<Vec<GlobalRegistryId>>,
}

/// Outcome of an insert attempt. `Collision` carries the existing
/// entry so the caller can emit an "already defined" diagnostic.
#[derive(Debug)]
pub enum InsertOutcome<'a> {
    Collision { existing: &'a RegistryEntry },
    Fresh(GlobalRegistryId),
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

    /// Seed a fresh registry with stdlib primitive stubs: scalar /
    /// FFI-width primitives plus `String` / `Binary` / `Bits`. All
    /// under the `Global` package so resolve never special-cases
    /// them. `CPtr<T>` and `Option<T>` are *not* stubbed here —
    /// they're defined in autoimported `Global.cptr` / `Global.kernel`
    /// sources and land through `collect` like any other user decl.
    ///
    /// Each primitive lands as `Struct(Some(empty_def))`: zero
    /// fields, empty conformance map. The empty definition lets
    /// `impl P for Int` blocks register conformances without the
    /// stub-vs-real-struct branching the bare-marker design
    /// originally forced.
    pub fn with_stdlib_stubs() -> Self {
        let mut reg = Self::default();
        for name in [
            "Int", "Bool", "Unit", "Float", "Never", "String", "Binary", "Bits", "Int8", "Int16",
            "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Float32", "Float64",
        ] {
            seed_primitive_stub(&mut reg, name, Vec::new());
        }
        reg
    }

    /// Register a constant in the `Constant(None)` state. The
    /// resolved type + value [`ConstantDefinition`] is stamped in
    /// later by [`Self::set_constant_definition`]. Constants don't
    /// take type parameters, so callers always pass an empty vec.
    pub fn insert_constant(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Constant(None), span, Vec::new())
    }

    /// Register an enum in the `Enum(None)` state. The resolved
    /// variant roster is stamped in later by
    /// [`Self::set_enum_definition`]. `type_params` carries the
    /// declared generic-param names from the AST so resolve and
    /// lift can answer "what params are in scope inside this decl?"
    /// before the variant payload types have been resolved.
    pub fn insert_enum(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Enum(None), span, type_params)
    }

    /// Register a function in the `Function(None)` state. The
    /// signature is stamped in later by [`Self::set_signature`].
    /// `type_params` carries the function's own declared generic
    /// params (not the enclosing struct/impl's; chained scopes are
    /// rebuilt at resolve time).
    pub fn insert_function(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Function(None), span, type_params)
    }

    /// Register a protocol in the `Protocol(None)` state. Method
    /// roster is stamped later by [`Self::set_protocol_definition`].
    pub fn insert_protocol(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Protocol(None), span, type_params)
    }

    /// Register a struct in the `Struct(None)` state. The
    /// resolved field layout is stamped in later by
    /// [`Self::set_struct_definition`].
    pub fn insert_struct(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Struct(None), span, type_params)
    }

    /// Register a `type X = ...` alias in the `TypeAlias(None)`
    /// state. The expansion is stamped in later by
    /// [`Self::set_type_alias_definition`]. Aliases don't take
    /// generic params today, so callers always pass an empty vec —
    /// generic aliases are tracked as a future language extension
    /// in the v1-parity plan.
    pub fn insert_type_alias(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::TypeAlias(None), span, Vec::new())
    }

    /// Stamp a resolved variant roster onto an enum entry. Panics
    /// unless the entry's kind is exactly `Enum(None)`.
    pub fn set_enum_definition(&mut self, id: GlobalRegistryId, definition: EnumDefinition) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!("set_enum_definition on missing registry id {id} — collect invariant violation")
        });
        match &entry.kind {
            GlobalKind::Enum(None) => {
                entry.kind = GlobalKind::Enum(Some(definition));
            }
            GlobalKind::Enum(Some(_)) => {
                panic!(
                    "set_enum_definition called twice on `{}` — lift_signatures must stamp \
                     each enum exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_enum_definition called on non-enum entry `{}` ({}) — \
                     only Enum entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Record `target_id` as conforming to `protocol_id` with the
    /// user-written `protocol_args` (e.g. `[String]` for
    /// `impl Eq<String> for User`). Returns the previously-recorded
    /// args if `target` already conformed to `protocol` — the
    /// caller emits a "duplicate `impl P for T`" diagnostic.
    /// Panics unless `target_id` names a struct or enum with a
    /// stamped definition (lift orders enum/struct definition
    /// stamping before impl conformance recording).
    pub fn record_conformance(
        &mut self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        protocol_args: Vec<ResolvedType>,
    ) -> Option<Vec<ResolvedType>> {
        let entry = self.entries.get_mut(&target_id).unwrap_or_else(|| {
            panic!(
                "record_conformance on missing registry id {target_id} — \
                 lift invariant violation",
            )
        });
        let conformances = match &mut entry.kind {
            GlobalKind::Struct(Some(def)) => &mut def.conformances,
            GlobalKind::Enum(Some(def)) => &mut def.conformances,
            other => panic!(
                "record_conformance on `{}` ({}) — only stamped struct/enum entries \
                 accept conformances",
                entry.identifier,
                other.label(),
            ),
        };
        if let Some(prev) = conformances.get(&protocol_id) {
            return Some(prev.clone());
        }
        conformances.insert(protocol_id, protocol_args);
        None
    }

    /// Whether `target_id` conforms to `protocol_id`. Returns the
    /// user-written protocol args (e.g. `[String]` for
    /// `Eq<String>`) when present, else `None`. O(1) HashMap
    /// lookup on the target's conformance index — IR's bounded
    /// dispatch never reaches this path (it goes straight to
    /// `[target, method_name]`); typecheck uses it for
    /// bound enforcement.
    pub fn lookup_conformance(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
    ) -> Option<&[ResolvedType]> {
        let entry = self.entries.get(&target_id)?;
        let conformances = match &entry.kind {
            GlobalKind::Struct(Some(def)) => &def.conformances,
            GlobalKind::Enum(Some(def)) => &def.conformances,
            _ => return None,
        };
        conformances.get(&protocol_id).map(Vec::as_slice)
    }

    /// Stamp a resolved method roster. Panics unless the entry's
    /// kind is exactly `Protocol(None)`.
    pub fn set_protocol_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: ProtocolDefinition,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_protocol_definition on missing registry id {id} — collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::Protocol(None) => {
                entry.kind = GlobalKind::Protocol(Some(definition));
            }
            GlobalKind::Protocol(Some(_)) => {
                panic!(
                    "set_protocol_definition called twice on `{}` — lift_signatures must stamp \
                     each protocol exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_protocol_definition called on non-protocol entry `{}` ({}) — \
                     only Protocol entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Stamp a resolved field layout onto a struct entry. Panics
    /// unless the entry's kind is exactly `Struct(None)`.
    pub fn set_struct_definition(&mut self, id: GlobalRegistryId, definition: StructDefinition) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_struct_definition on missing registry id {id} — collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::Struct(None) => {
                entry.kind = GlobalKind::Struct(Some(definition));
            }
            GlobalKind::Struct(Some(_)) => {
                panic!(
                    "set_struct_definition called twice on `{}` — lift_signatures must stamp \
                     each struct exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_struct_definition called on non-struct entry `{}` ({}) — \
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
                identifier,
                kind,
                span,
                type_params,
                type_param_bounds,
            },
        );
        InsertOutcome::Fresh(id)
    }

    /// Stamp a resolved type + RHS onto a constant entry. Panics
    /// unless the entry's kind is exactly `Constant(None)`.
    pub fn set_constant_definition(
        &mut self,
        id: GlobalRegistryId,
        definition: ConstantDefinition,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_constant_definition on missing registry id {id} — collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::Constant(None) => {
                entry.kind = GlobalKind::Constant(Some(Box::new(definition)));
            }
            GlobalKind::Constant(Some(_)) => {
                panic!(
                    "set_constant_definition called twice on `{}` — lift_signatures must stamp \
                     each constant exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_constant_definition called on non-constant entry `{}` ({}) — \
                     only Constant entries carry definitions",
                    entry.identifier,
                    other.label(),
                );
            }
        }
    }

    /// Stamp a resolved expansion onto a type-alias entry. Panics
    /// unless the entry's kind is exactly `TypeAlias(None)`.
    pub fn set_type_alias_definition(&mut self, id: GlobalRegistryId, expansion: ResolvedType) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_type_alias_definition on missing registry id {id} — \
                 collect invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::TypeAlias(None) => {
                entry.kind = GlobalKind::TypeAlias(Some(expansion));
            }
            GlobalKind::TypeAlias(Some(_)) => {
                panic!(
                    "set_type_alias_definition called twice on `{}` — \
                     lift_type_aliases must stamp each alias exactly once",
                    entry.identifier,
                );
            }
            other => {
                panic!(
                    "set_type_alias_definition called on non-alias entry `{}` ({}) — \
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
    /// not a `TypeAlias` entry — only the cycle pass should call
    /// this.
    pub fn set_type_alias_definition_force(
        &mut self,
        id: GlobalRegistryId,
        expansion: ResolvedType,
    ) {
        let entry = self.entries.get_mut(&id).unwrap_or_else(|| {
            panic!(
                "set_type_alias_definition_force on missing registry id {id} — \
                 lift invariant violation"
            )
        });
        match &entry.kind {
            GlobalKind::TypeAlias(_) => {
                entry.kind = GlobalKind::TypeAlias(Some(expansion));
            }
            other => panic!(
                "set_type_alias_definition_force called on non-alias entry `{}` ({}) — \
                 only TypeAlias entries support force-stamp",
                entry.identifier,
                other.label(),
            ),
        }
    }

    /// Stamp a resolved signature onto a function entry. Panics unless
    /// the entry's kind is exactly `Function(None)`.
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

    /// Build a leaf [`ResolvedType`] pointing at the preloaded
    /// `Global.<name>` stdlib stub. Panics if the stub is missing —
    /// preload is a [`Self::with_stdlib_stubs`] invariant.
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
                "stdlib stub `Global.{name}` missing from registry — \
                 alpha pipeline must seed it via `GlobalRegistry::with_stdlib_stubs`",
            )
        });
        ResolvedType::leaf(Resolution::Global(id))
    }

    /// Build the [`ResolvedType`] for a primitive literal — the
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
    /// is out of range (compiler bug — index should have come from
    /// a [`Resolution::TypeParam`] anchored to the same owner).
    pub fn type_param_name(&self, owner: GlobalRegistryId, index: TypeParamIndex) -> Option<&str> {
        self.get(owner)?
            .type_params
            .get(index.as_u32() as usize)
            .map(String::as_str)
    }

    /// Slice of generic-decl param names declared on `owner`. `None`
    /// when `owner` is unknown; a known owner with no generics
    /// returns `Some(&[])`. Used by
    /// [`crate::pipeline::lift_signatures::types::TypeParamScope::lookup`]
    /// to walk a chained scope and turn a name into
    /// `(owner, TypeParamIndex)`.
    pub fn type_params(&self, owner: GlobalRegistryId) -> Option<&[String]> {
        self.get(owner).map(|entry| entry.type_params.as_slice())
    }

    /// Slice of resolved bounds on `owner`'s generic-decl params,
    /// parallel to [`Self::type_params`] (same length, same indexing).
    /// Inner vec is the `&`-composed protocol-id list for that param;
    /// empty means unbounded. `None` when `owner` is unknown.
    pub fn type_param_bounds(&self, owner: GlobalRegistryId) -> Option<&[Vec<GlobalRegistryId>]> {
        self.get(owner)
            .map(|entry| entry.type_param_bounds.as_slice())
    }

    /// Replace `owner`'s `type_param_bounds`. `bounds.len()` must equal
    /// the entry's `type_params.len()`. Called by lift's bounds-resolve
    /// sub-pass after every protocol id is registered.
    pub fn set_type_param_bounds(
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
    /// runs; callers needing a deterministic order sort by id (matches
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
    /// has been collected) is silently skipped — callers should only
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

/// Protocols that every type implicitly satisfies — the synthesizer
/// (or hand-written stdlib impls) guarantee a `Debug` impl exists
/// for every concrete monomorphization, so a bare type-parameter
/// `T.format()` can resolve as if `T: Debug` were declared. Equality
/// / Hash join this list once they're auto-derived.
pub const UNIVERSAL_PROTOCOLS: &[&str] = &["Debug"];

/// Seed a primitive struct stub under `Global.<name>` with an empty
/// `StructDefinition` (no fields, no conformances). The empty
/// definition lets `impl P for <name>` blocks call
/// [`GlobalRegistry::record_conformance`] without distinguishing
/// stubs from user structs.
fn seed_primitive_stub(reg: &mut GlobalRegistry, name: &str, type_params: Vec<String>) {
    let outcome = reg.insert_struct(
        Identifier::new("Global", vec![name.to_string()]),
        Span::default(),
        type_params,
    );
    let id = match outcome {
        InsertOutcome::Fresh(id) => id,
        InsertOutcome::Collision { existing } => panic!(
            "stdlib stub `Global.{name}` collided on preload with `{}` — \
             registry was not empty",
            existing.identifier,
        ),
    };
    reg.set_struct_definition(
        id,
        StructDefinition {
            conformances: BTreeMap::new(),
            fields: Vec::new(),
        },
    );
}
