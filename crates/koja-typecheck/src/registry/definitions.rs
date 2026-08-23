//! Resolved-data carriers stamped onto [`super::GlobalKind`]
//! payloads by `lift_signatures`. The registry's storage / insert
//! API lives in [`super`]. This module is the per-decl
//! "what-is-the-shape-of-X" surface: `FunctionSignature`,
//! `StructDefinition`, `EnumDefinition`, `ProtocolDefinition`, and
//! the small `Resolved*` leaves they're built from.
//!
//! Splitting these out keeps [`super`] focused on the
//! [`super::GlobalRegistry`] container itself. Downstream consumers
//! re-export the same types unchanged through
//! [`crate`]'s public surface.

use std::collections::BTreeMap;

use koja_ast::ast::Expr;
use koja_ast::identifier::{GlobalRegistryId, ResolvedType};

/// The compiler-provided representation behind a `builtin` type
/// declaration. IR maps each shape structurally to its `IRType`, so
/// lowering never re-derives the shape from the type's name. Two
/// entries may share a shape: `Int` and `Int64` are distinct
/// registry entries with the same representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinShape {
    Binary,
    Bits,
    Bool,
    CPtr,
    Float32,
    Float64,
    Int8,
    Int16,
    Int32,
    Int64,
    List,
    Map,
    Never,
    Set,
    String,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Unit,
}

impl BuiltinShape {
    /// The exact number of type parameters a declaration of this
    /// shape must carry.
    pub fn arity(self) -> usize {
        match self {
            Self::Map => 2,
            Self::CPtr | Self::List | Self::Set => 1,
            _ => 0,
        }
    }

    /// Whether `==` on two values of this shape stays on the
    /// backend's primitive fast path instead of rewriting to
    /// `.equals?(...)` method dispatch.
    pub fn has_primitive_equality(self) -> bool {
        match self {
            Self::Bool
            | Self::Float32
            | Self::Float64
            | Self::Int8
            | Self::Int16
            | Self::Int32
            | Self::Int64
            | Self::String
            | Self::UInt8
            | Self::UInt16
            | Self::UInt32
            | Self::UInt64 => true,
            Self::Binary
            | Self::Bits
            | Self::CPtr
            | Self::List
            | Self::Map
            | Self::Never
            | Self::Set
            | Self::Unit => false,
        }
    }
}

/// Extra bounds a conditional impl grants its target's params while
/// code inside that impl resolves. `impl Encodable for
/// List<T: Encodable>` grants `TypeParam(List, 0): Encodable` to its
/// own body, so obligations raised there can discharge through the
/// impl's condition instead of the target's declared bounds.
#[derive(Clone, Debug)]
pub struct BoundOverlay {
    /// Parallel to the owner's params, protocol bounds per slot.
    pub bounds: Vec<Vec<ResolvedProtocolBound>>,
    /// The target type whose params the bounds attach to.
    pub owner: GlobalRegistryId,
}

/// A protocol obligation with its resolved user-declared arguments.
///
/// `E: Enumeration<T>` records the `Enumeration` registry id and
/// `[TypeParam(T)]`. A bare bound records an empty argument list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProtocolBound {
    pub args: Vec<ResolvedType>,
    pub protocol_id: GlobalRegistryId,
}

/// One recorded `target : protocol` fact, stamped by lift onto the
/// target's struct/enum/builtin definition. `scope` says which
/// instantiations of the target the fact covers, so `impl P for
/// Bag<Int>` and `impl P for Bag<T>` record distinguishable facts.
#[derive(Clone, Debug)]
pub struct Conformance {
    /// Protocol args as written on the impl (e.g. `[C, M, R]` for
    /// `Process`). May mention the target's own params only when
    /// `scope` is `Parameterized`.
    pub protocol_args: Vec<ResolvedType>,
    pub scope: ConformanceScope,
}

/// Which instantiations of the target a [`Conformance`] covers.
/// Classified once at lift time, never re-derived from arg shapes.
#[derive(Clone, Debug)]
pub enum ConformanceScope {
    /// Covers exactly these target args, as in `impl P for Bag<Int>`.
    /// Impls on non-generic targets record as `Concrete(vec![])`, so
    /// `Parameterized` only ever appears on generic targets.
    Concrete(Vec<ResolvedType>),
    /// Parameterized over the target's own params. Written as
    /// `impl P for Bag<T>` or a header conformance on a generic
    /// type. `bounds` parallels the target's params. A conditional
    /// conformance like `impl Equality for List<T: Equality>`
    /// records the required protocol bounds per slot, and an empty
    /// inner vec means that slot is unconditional, so an all-empty
    /// `bounds` covers every instantiation.
    Parameterized {
        bounds: Vec<Vec<ResolvedProtocolBound>>,
    },
}

/// Payload of a [`super::GlobalKind::Builtin`] entry. Unlike the
/// other definition carriers there is no `None` lifecycle state:
/// the compiler owns the definition, so the shape is stamped at
/// seed time. `conformances` mirrors
/// [`StructDefinition::conformances`] so `impl P for Int` works the
/// same as on user structs.
#[derive(Clone, Debug)]
pub struct BuiltinDefinition {
    pub conformances: BTreeMap<GlobalRegistryId, Vec<Conformance>>,
    pub shape: BuiltinShape,
}

/// How a function call dispatches on its callee.
///
/// `Static` is the default: direct lookup by qualified name. The
/// argument list is exactly what the caller wrote. `Instance` requires
/// a receiver value whose static type matches the enclosing struct.
/// The receiver becomes the implicit first argument and the caller's
/// explicit args populate `params[1..]`.
///
/// Orthogonal to [`crate::FunctionKind`] (which describes how a body
/// is materialized at codegen: `Regular` vs `Intrinsic`). A function
/// is one of `{Regular, Intrinsic} × {Static, Instance}`. Keeping the
/// axes as separate enums avoids combinatorial pattern matches at
/// every call site that cares about only one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Instance,
    Static,
}

/// A single resolved parameter: surface-syntax name and resolved
/// type, stamped by `lift_signatures` off the matching
/// `Param::{Regular,Self_}` variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedParam {
    pub name: String,
    pub ty: ResolvedType,
}

/// One field of a [`StructDefinition`]. Surface-syntax name plus the
/// fully-resolved field type as stamped by `lift_signatures`.
///
/// `default` holds the unresolved default-value AST when the field
/// declares one. Construction sites that omit the field clone and
/// re-resolve it against the substituted field type, so generic
/// defaults (`Option.None`, `[]`) work per site.
#[derive(Clone, Debug)]
pub struct ResolvedStructField {
    pub default: Option<Box<Expr>>,
    pub name: String,
    pub ty: ResolvedType,
}

/// A fully-resolved function signature stamped onto
/// [`super::GlobalKind::Function`] entries by the `lift_signatures`
/// sub-pass. Params and return carry registry-backed [`ResolvedType`]s,
/// so a signature stays valid as long as its referents do.
///
/// `dispatch` distinguishes static (free or `Type.method`) calls from
/// instance (`receiver.method`) calls. `lift_signatures` sets
/// [`Dispatch::Instance`] when the function declares a `Param::Self_`
/// first parameter. Everything else stays [`Dispatch::Static`].
///
/// `impl_args` carries the concrete pinning of a partial-spec impl
/// block (`impl CPtr<UInt8>` -> `[UInt8]`). Empty for top-level
/// functions, inline struct/enum methods, and generic-pinned impl
/// blocks (`impl Bag<T>`). Set only when every arg of the impl
/// target is fully resolved (no `TypeParam`s). Lower consults this
/// to mangle bare static calls between siblings inside a
/// concrete-pinned impl as `<Type>_$Args$.method`, matching what
/// monomorphization produces from the receiver side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    /// True when the author spelled the return as `-> T ! E`. The
    /// `return_type` already holds the lifted `Result<T, E>`, and this
    /// flag gates Ok-wrapping, which never applies to functions
    /// declared with an explicit `-> Result<T, E>`.
    pub declared_fallible: bool,
    pub dispatch: Dispatch,
    pub params: Vec<ResolvedParam>,
    pub return_type: ResolvedType,
    pub impl_args: Vec<ResolvedType>,
}

/// Field layout + protocol conformances for a user-declared struct.
/// Stamped onto a [`super::GlobalKind::Struct`] entry by the
/// `lift_signatures` sub-pass. Field order matches declaration
/// order. Downstream consumers (IR lower, codegen) index by
/// position. Generic-decl param names live on the
/// [`super::RegistryEntry`] itself, not here, so the registry can
/// answer "what params does this owner declare?" mid-lift.
///
/// `conformances` is the per-target index of which protocols the
/// struct implements: `protocol_id -> [Conformance]`, one record
/// per impl of that protocol (a parameterized impl, or one or more
/// concrete instantiations). The methods themselves register at
/// `[target_head, method_name]`, so dispatch is `[target,
/// method_name]` directly. IR doesn't walk this map. Typecheck
/// consults it for bound enforcement (slice 2.3) and duplicate-impl
/// detection.
///
/// Transitional note: this representation lets a struct/enum
/// entry be self-contained for IR consumption: IR never has to
/// walk a separate impl table. A future incremental-cache pass
/// may want a richer structural index over `(target, protocol)`
/// pairs (e.g. for cross-package resolution). Revisit then.
#[derive(Clone, Debug)]
pub struct StructDefinition {
    pub fields: Vec<ResolvedStructField>,
    pub conformances: BTreeMap<GlobalRegistryId, Vec<Conformance>>,
}

/// Variant roster + protocol conformances for a user-declared
/// enum. Stamped onto a [`super::GlobalKind::Enum`] entry by the
/// `lift_signatures` sub-pass. Variant order matches declaration
/// order: the IR's discriminant tag is the variant's position in
/// this vec, and downstream consumers (IR lower, codegen) index by
/// position. Generic-decl param names live on the
/// [`super::RegistryEntry`] itself.
///
/// See [`StructDefinition::conformances`] for the conformance-map
/// shape and rationale.
#[derive(Clone, Debug)]
pub struct EnumDefinition {
    pub variants: Vec<ResolvedEnumVariant>,
    pub conformances: BTreeMap<GlobalRegistryId, Vec<Conformance>>,
}

/// One variant on an [`EnumDefinition`]. `name` is the surface
/// identifier (`Some` in `Option.Some`). `data` carries the variant's
/// payload shape: empty for unit variants, positional types for
/// tuple variants, named fields for struct variants.
#[derive(Clone, Debug)]
pub struct ResolvedEnumVariant {
    pub data: ResolvedVariantData,
    pub name: String,
}

/// Payload shape of an enum variant.
///
/// The `Struct` arm reuses [`ResolvedStructField`] verbatim: a
/// struct variant's payload layout is structurally a struct, and the
/// shared shape lets the validation helpers in `resolve/structs.rs`
/// be reused for both struct construction and struct-variant
/// construction without duplicating the per-field walk.
#[derive(Clone, Debug)]
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
/// a default body exists in lift's body sidecar. The body itself is
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
/// onto a [`super::GlobalKind::Constant`] entry by the
/// `lift_signatures` sub-pass after the RHS shape is validated and
/// resolved.
///
/// The registry intentionally holds the AST `Expr` rather than a
/// projected literal payload: lift restricts the surface to literals,
/// negated numerics, unit enum variants, and structs of literals, but
/// IR lower wants the original `Expr`'s `resolution` data (struct id,
/// variant tag) to canonicalize the pool entry. Storing the AST node
/// keeps that information in one place. Registry consumers walk it
/// the same way they'd walk a literal at the use site.
#[derive(Clone, Debug)]
pub struct ConstantDefinition {
    pub ty: ResolvedType,
    pub value: Expr,
}

impl StructDefinition {
    /// Lookup a field by name. Returns `Some((index, &field))` for a
    /// match, `None` otherwise. Linear scan: struct field counts
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
    /// Lookup a variant by name. Returns `Some((index, &variant))`
    /// for a match, `None` otherwise. Linear scan: variant counts
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
