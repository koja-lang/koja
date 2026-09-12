//! Enum-shaped top-level decls and the per-instruction payload for
//! enum-variant construction.
//!
//! A lowered [`IREnumDecl`] keys at its [`IRSymbol`] (mangled
//! package-qualified name, mirroring [`crate::IRStructDecl`]) and
//! carries variant metadata in declaration order. Each
//! [`IREnumVariant`] carries an [`IRVariantTag`] equal to its
//! 0-based position, so backends index by position. The `u8` tag
//! caps the variant count at 256.
//!
//! The [`IRVariantPayload::Struct`] arm and the
//! [`EnumPayloadInit::Struct`] arm reuse [`crate::IRStructField`]
//! and [`crate::StructFieldInit`], since a struct variant's payload
//! is structurally a struct. The seal helpers for structs then apply
//! to struct-variant payloads as is.
//!
//! Storage layout belongs to the backends.

use crate::function::IRSymbol;
use crate::struct_decl::{IRStructField, StructFieldInit};
use crate::types::{IRType, ValueId};

/// Discriminant tag for an enum variant, equal to its 0-based
/// declaration position. The `u8` width is the ABI tag width, so
/// lowering rejects enums with more than 256 variants. `Display`
/// renders `#<n>` to match the `bb<n>` / `%<n>` IR text format.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRVariantTag(pub u8);

impl std::fmt::Display for IRVariantTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A lowered enum declaration. `symbol` is the same package-qualified
/// mangled name shape an [`crate::IRFunction`] uses, and `variants` is
/// the declaration-order variant list. Variant order *is* the tag
/// (variant `i` has `tag == IRVariantTag(i as u8)`), so seal
/// asserts dense, declaration-ordered tags. Generic decls never
/// appear here, as [`crate::generics::instantiate`] produces one
/// [`IREnumDecl`] per discovered instantiation, keyed at its
/// mangled symbol.
#[derive(Debug, Clone)]
pub struct IREnumDecl {
    pub symbol: IRSymbol,
    pub variants: Vec<IREnumVariant>,
}

/// One variant of an [`IREnumDecl`]. `name` is the surface variant
/// name (`Some` in `Option.Some`), `payload` carries the variant's
/// data shape, and `tag` is the discriminant byte (== position in
/// `variants`, asserted by seal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IREnumVariant {
    pub name: String,
    pub payload: IRVariantPayload,
    pub tag: IRVariantTag,
}

/// Payload shape of an enum variant, mirroring the typecheck-layer
/// `ResolvedVariantData` shape. The `Struct` arm reuses
/// [`IRStructField`] (already in declaration order with positional
/// indices) so the seal helpers shared with [`crate::IRStructDecl`]
/// apply unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IRVariantPayload {
    Struct(Vec<IRStructField>),
    Tuple(Vec<IRType>),
    Unit,
}

/// Per-instruction payload init for [`crate::IRInstruction::EnumConstruct`].
/// Mirrors [`IRVariantPayload`] one-to-one but carries
/// already-lowered [`ValueId`]s instead of declared types. The
/// `Struct` arm reuses [`StructFieldInit`], keeping the same
/// canonicalization invariant the struct slice already maintains:
/// indices are declaration-ordered with one entry per declared field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumPayloadInit {
    Struct(Vec<StructFieldInit>),
    Tuple(Vec<ValueId>),
    Unit,
}
