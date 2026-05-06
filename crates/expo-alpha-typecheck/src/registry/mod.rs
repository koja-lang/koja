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

use std::collections::HashMap;

use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex};
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

/// Field layout for a user-declared struct. Stamped onto a
/// [`GlobalKind::Struct`] entry by the `lift_signatures` sub-pass.
/// Field order matches declaration order — downstream consumers
/// (IR lower, codegen) index by position. `type_params` is non-empty
/// for generic structs (`struct Pair<T, U>`); each name is in scope
/// inside the field types and resolves through
/// [`expo_ast::identifier::Resolution::TypeParam`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructDefinition {
    pub fields: Vec<ResolvedStructField>,
    pub type_params: Vec<String>,
}

/// Variant roster for a user-declared enum. Stamped onto a
/// [`GlobalKind::Enum`] entry by the `lift_signatures` sub-pass.
/// Variant order matches declaration order — the IR's discriminant
/// tag is the variant's position in this vec, and downstream
/// consumers (IR lower, codegen) index by position. `type_params`
/// is non-empty for generic enums (`enum Result<T, E>`); each name
/// is in scope inside variant payload types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumDefinition {
    pub type_params: Vec<String>,
    pub variants: Vec<ResolvedEnumVariant>,
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

    /// Render the name of a type parameter by its anchored index.
    /// `None` if the index is out of range (compiler bug).
    pub fn type_param_name(&self, index: TypeParamIndex) -> Option<&str> {
        self.type_params
            .get(index.as_u32() as usize)
            .map(String::as_str)
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

    /// Render the name of a type parameter by its anchored index.
    /// `None` if the index is out of range (compiler bug).
    pub fn type_param_name(&self, index: TypeParamIndex) -> Option<&str> {
        self.type_params
            .get(index.as_u32() as usize)
            .map(String::as_str)
    }
}

/// What kind of declaration a registry entry points at.
///
/// `Enum`, `Function`, `Protocol`, and `Struct` entries carry their
/// lifted payload inline as `Option<_>`: `None` is the "collected
/// but not yet lifted" state (and the permanent state for stdlib
/// stub primitives), `Some(_)` the lifted state reached after
/// `lift_signatures` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalKind {
    Enum(Option<EnumDefinition>),
    Function(Option<FunctionSignature>),
    Protocol(Option<ProtocolDefinition>),
    Struct(Option<StructDefinition>),
}

impl GlobalKind {
    pub fn label(&self) -> &'static str {
        match self {
            GlobalKind::Enum(_) => "enum",
            GlobalKind::Function(_) => "function",
            GlobalKind::Protocol(_) => "protocol",
            GlobalKind::Struct(_) => "struct",
        }
    }
}

/// A single registered declaration: canonical [`Identifier`],
/// [`GlobalKind`], and source span (used for "already defined here"
/// diagnostic notes).
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub identifier: Identifier,
    pub kind: GlobalKind,
    pub span: Span,
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
    /// `Float`/`String`). They register as ordinary
    /// [`GlobalKind::Struct`] entries under the `Global` package so
    /// resolve never special-cases primitives.
    ///
    /// Temporary scaffolding — once the real stdlib compiles as a
    /// package these entries land through `collect` like any other
    /// decl. Stubs share their shape with the eventual real entries,
    /// so the cutover is invisible to downstream consumers.
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

    /// Register an enum in the `Enum(None)` state. The resolved
    /// variant roster is stamped in later by
    /// [`Self::set_enum_definition`].
    pub fn insert_enum(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Enum(None), span)
    }

    /// Register a function in the `Function(None)` state. The
    /// signature is stamped in later by [`Self::set_signature`].
    pub fn insert_function(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Function(None), span)
    }

    /// Register a protocol in the `Protocol(None)` state. Method
    /// roster is stamped later by [`Self::set_protocol_definition`].
    pub fn insert_protocol(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Protocol(None), span)
    }

    /// Register a struct in the `Struct(None)` state. The
    /// resolved field layout is stamped in later by
    /// [`Self::set_struct_definition`]; preloaded stdlib stub
    /// primitives stay in `Struct(None)` permanently.
    pub fn insert_struct(&mut self, identifier: Identifier, span: Span) -> InsertOutcome<'_> {
        self.insert(identifier, GlobalKind::Struct(None), span)
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
    /// `(owner, index)`. Hides the `GlobalKind::{Struct,Enum}` dispatch
    /// from rendering call sites. `None` when `owner` doesn't resolve
    /// to a generic struct/enum or `index` is out of range
    /// (compiler bug).
    pub fn type_param_name(
        &self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    ) -> Option<&str> {
        let entry = self.get(owner)?;
        match &entry.kind {
            GlobalKind::Enum(Some(definition)) => definition.type_param_name(index),
            GlobalKind::Struct(Some(definition)) => definition.type_param_name(index),
            _ => None,
        }
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
