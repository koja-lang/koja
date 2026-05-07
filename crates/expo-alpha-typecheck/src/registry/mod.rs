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

use expo_ast::ast::Expr;
use expo_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::span::Span;

mod format;

pub use format::format_registry;

/// How a function call dispatches on its callee.
///
/// `Static` is the default — direct lookup by qualified name; the
/// argument list is exactly what the caller wrote. `Instance` requires
/// a receiver value whose static type matches the enclosing struct;
/// the receiver becomes the implicit first argument and the caller's
/// explicit args populate `params[1..]`.
///
/// Orthogonal to [`crate::FunctionKind`] (which describes how a body
/// is materialized at codegen — `Regular` vs `Intrinsic`). A function
/// is one of `{Regular, Intrinsic} × {Static, Instance}`; keeping the
/// axes as separate enums avoids combinatorial pattern matches at
/// every call site that cares about only one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Instance,
    Static,
}

/// A single resolved parameter: surface-syntax name plus resolved type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedParam {
    pub name: String,
    pub ty: ResolvedType,
}

/// One field of a [`StructDefinition`]. Surface-syntax name plus the
/// fully-resolved field type as stamped by `lift_signatures`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedStructField {
    pub name: String,
    pub ty: ResolvedType,
}

/// A fully-resolved function signature stamped onto
/// [`GlobalKind::Function`] entries by the `lift_signatures` sub-pass.
/// Params and return carry registry-backed [`ResolvedType`]s, so a
/// signature stays valid as long as its referents do.
///
/// `dispatch` distinguishes static (free or `Type.method`) calls from
/// instance (`receiver.method`) calls. `lift_signatures` sets
/// [`Dispatch::Instance`] when the function declares a `Param::Self_`
/// first parameter; everything else stays [`Dispatch::Static`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub dispatch: Dispatch,
    pub params: Vec<ResolvedParam>,
    pub return_type: ResolvedType,
}

/// Field layout + protocol conformances for a user-declared struct.
/// Stamped onto a [`GlobalKind::Struct`] entry by the
/// `lift_signatures` sub-pass. Field order matches declaration
/// order — downstream consumers (IR lower, codegen) index by
/// position. Generic-decl param names live on the
/// [`RegistryEntry`] itself, not here, so the registry can answer
/// "what params does this owner declare?" mid-lift.
///
/// `conformances` is the per-target index of which protocols the
/// struct implements: `protocol_id -> user-written protocol args`
/// (e.g. `Eq -> [String]` for `impl Eq<String> for User`). The
/// methods themselves register at `[target_head, method_name]`,
/// so dispatch is `[target, method_name]` directly — IR doesn't
/// walk this map. Typecheck consults it for bound enforcement
/// (slice 2.3) and duplicate-impl detection.
///
/// Transitional note: this representation lets a struct/enum
/// entry be self-contained for IR consumption — IR never has to
/// walk a separate impl table. A future incremental-cache pass
/// may want a richer structural index over `(target, protocol)`
/// pairs (e.g. for cross-package resolution); revisit then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructDefinition {
    pub fields: Vec<ResolvedStructField>,
    pub conformances: BTreeMap<GlobalRegistryId, Vec<ResolvedType>>,
}

/// Variant roster + protocol conformances for a user-declared
/// enum. Stamped onto a [`GlobalKind::Enum`] entry by the
/// `lift_signatures` sub-pass. Variant order matches declaration
/// order — the IR's discriminant tag is the variant's position in
/// this vec, and downstream consumers (IR lower, codegen) index by
/// position. Generic-decl param names live on the
/// [`RegistryEntry`] itself.
///
/// See [`StructDefinition::conformances`] for the conformance-map
/// shape and rationale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumDefinition {
    pub variants: Vec<ResolvedEnumVariant>,
    pub conformances: BTreeMap<GlobalRegistryId, Vec<ResolvedType>>,
}

/// One variant on an [`EnumDefinition`]. `name` is the surface
/// identifier (`Some` in `Option.Some`); `data` carries the variant's
/// payload shape — empty for unit variants, positional types for
/// tuple variants, named fields for struct variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEnumVariant {
    pub data: ResolvedVariantData,
    pub name: String,
}

/// Payload shape of an enum variant.
///
/// The `Struct` arm reuses [`ResolvedStructField`] verbatim — a
/// struct variant's payload layout is structurally a struct, and the
/// shared shape lets the validation helpers in `resolve/structs.rs`
/// be reused for both struct construction and struct-variant
/// construction without duplicating the per-field walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedVariantData {
    Struct(Vec<ResolvedStructField>),
    Tuple(Vec<ResolvedType>),
    Unit,
}

/// Method roster for a user-declared protocol, stamped by
/// `lift_signatures`. Method order matches declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolDefinition {
    pub methods: Vec<ResolvedProtocolMethod>,
}

/// One method on a [`ProtocolDefinition`]. `has_default` flags whether
/// a default body exists in lift's body sidecar; the body itself is
/// not stored here (registry holds resolved types only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProtocolMethod {
    pub dispatch: Dispatch,
    pub has_default: bool,
    pub name: String,
    pub non_self_params: Vec<ResolvedParam>,
    pub return_type: ResolvedType,
}

/// Resolved value for a package-level `const NAME = expr`. Stamped
/// onto a [`GlobalKind::Constant`] entry by the `lift_signatures`
/// sub-pass after the RHS shape is validated and resolved.
///
/// The registry intentionally holds the AST `Expr` rather than a
/// projected literal payload: lift restricts the surface to literals,
/// negated numerics, unit enum variants, and structs of literals, but
/// IR lower wants the original `Expr`'s `resolution` data (struct id,
/// variant tag) to canonicalize the pool entry. Storing the AST node
/// keeps that information in one place — registry consumers walk it
/// the same way they'd walk a literal at the use site.
#[derive(Clone, Debug)]
pub struct ConstantDefinition {
    pub ty: ResolvedType,
    pub value: Expr,
}

impl StructDefinition {
    /// Lookup a field by name; returns `Some((index, &field))` for a
    /// match, `None` otherwise. Linear scan — struct field counts
    /// are small (single-digit typical, two-digit max), so the
    /// constant factor wins over a hashmap. Used by `resolve` to
    /// turn `expr.field` into an index + type.
    pub fn lookup_field(&self, name: &str) -> Option<(u32, &ResolvedStructField)> {
        self.fields
            .iter()
            .enumerate()
            .find(|(_, field)| field.name == name)
            .map(|(index, field)| (index as u32, field))
    }
}

impl EnumDefinition {
    /// Lookup a variant by name; returns `Some((index, &variant))`
    /// for a match, `None` otherwise. Linear scan — variant counts
    /// are small (single-digit typical, capped at 256 by the `i8`
    /// discriminant tag width), so the constant factor wins over a
    /// hashmap. Used by `resolve` to turn `Color.Red` into a tag +
    /// payload shape and by IR lower to compute the discriminant.
    pub fn lookup_variant(&self, name: &str) -> Option<(u32, &ResolvedEnumVariant)> {
        self.variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == name)
            .map(|(index, variant)| (index as u32, variant))
    }
}

/// What kind of declaration a registry entry points at.
///
/// Most variants carry their lifted payload inline as `Option<_>`:
/// `None` is the "collected but not yet lifted" state (and the
/// permanent state for stdlib stub primitives), `Some(_)` the lifted
/// state reached after `lift_signatures` runs. [`GlobalKind::Constant`]
/// boxes its `Some(_)` payload so this enum stays a reasonable size despite
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
}

impl GlobalKind {
    pub fn label(&self) -> &'static str {
        match self {
            GlobalKind::Constant(_) => "constant",
            GlobalKind::Enum(_) => "enum",
            GlobalKind::Function(_) => "function",
            GlobalKind::Protocol(_) => "protocol",
            GlobalKind::Struct(_) => "struct",
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

    /// Seed a fresh registry with stdlib struct stubs for the scalar
    /// types alpha synthesizes from literals (`Int`/`Bool`/`Unit`/
    /// `Float`/`String`), the explicit-width numeric primitives that
    /// FFI signatures admit (`Int8`..`Int64`, `UInt8`..`UInt64`,
    /// `Float32`/`Float64`), and the generic `CPtr<T>` pointer wrapper.
    /// They register as ordinary [`GlobalKind::Struct`] entries under
    /// the `Global` package so resolve never special-cases primitives.
    ///
    /// Temporary scaffolding — once the real stdlib compiles as a
    /// package these entries land through `collect` like any other
    /// decl. Stubs share their shape with the eventual real entries,
    /// so the cutover is invisible to downstream consumers.
    pub fn with_stdlib_stubs() -> Self {
        let mut reg = Self::default();
        for name in [
            "Int", "Bool", "Unit", "Float", "String", "Int8", "Int16", "Int32", "Int64", "UInt8",
            "UInt16", "UInt32", "UInt64", "Float32", "Float64",
        ] {
            let outcome = reg.insert_struct(
                Identifier::new("Global", vec![name.to_string()]),
                Span::default(),
                Vec::new(),
            );
            debug_assert!(
                matches!(outcome, InsertOutcome::Fresh(_)),
                "stdlib stub `Global.{name}` collided on preload — registry was not empty",
            );
        }
        let cptr_outcome = reg.insert_struct(
            Identifier::new("Global", vec!["CPtr".to_string()]),
            Span::default(),
            vec!["T".to_string()],
        );
        debug_assert!(
            matches!(cptr_outcome, InsertOutcome::Fresh(_)),
            "stdlib stub `Global.CPtr` collided on preload — registry was not empty",
        );
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
    /// [`Self::set_struct_definition`]; preloaded stdlib stub
    /// primitives stay in `Struct(None)` permanently.
    pub fn insert_struct(
        &mut self,
        identifier: Identifier,
        span: Span,
        type_params: Vec<String>,
    ) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Struct(None), span, type_params)
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
    /// unless the entry's kind is exactly `Struct(None)` — preloaded
    /// stdlib stubs are bare markers and don't accept a definition.
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
    /// Used by `lift_signatures` (synthesizing parameter / return
    /// types from `TypeExpr::Unit` and `TypeExpr::Named`) and by
    /// `resolve` (stamping literal types). Both consumers want the
    /// same panic-on-miss semantics, so the helper lives here rather
    /// than being duplicated per pass.
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
}
