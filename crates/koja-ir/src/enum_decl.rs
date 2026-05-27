//! Enum-shaped top-level decls and the per-instruction payload for
//! enum-variant construction.
//!
//! A lowered [`IREnumDecl`] keys at its [`IRSymbol`] (mangled
//! package-qualified name, mirroring [`crate::IRStructDecl`]) and
//! carries variant metadata in declaration order. Each
//! [`IREnumVariant`] carries an [`IRVariantTag`] equal to its
//! 0-based position; the tag width caps the variant count at 256
//! (the LLVM layout uses an `i8` discriminant). Variant order
//! matches declaration order so eval / LLVM index by position.
//!
//! The [`IRVariantPayload::Struct`] arm and the
//! [`EnumPayloadInit::Struct`] arm intentionally reuse
//! [`crate::IRStructField`] and [`crate::StructFieldInit`] — a
//! struct variant's payload layout is structurally a struct, and
//! the construction-site init is structurally a `StructInit::fields`.
//! Reusing keeps the seal helpers (dense-index / unique-name /
//! supported-type checks, init canonicalization) usable as-is for
//! struct-variant payloads without duplicating the validation
//! pipeline.
//!
//! ## LLVM layout (Rust-style)
//!
//! Each enum gets three families of LLVM types:
//!
//! - `%<enum>` (outer): an opaque blob sized + aligned to fit the
//!   largest complete variant struct. `{ [N x i<max_align*8>] }` —
//!   the `iN` chunks are the trick that gives LLVM "this storage is
//!   aligned to max_align" (a plain `[M x i8]` is alignment-1
//!   regardless of size).
//! - `%<enum>.<variant>` (per-variant complete): non-packed
//!   `{ i8 tag, [pad x i8] padding, %<enum>.<variant>.payload }`,
//!   or just `{ i8 tag }` for Unit. The padding aligns the payload
//!   struct to its natural alignment so each payload field lands at
//!   a properly-aligned offset.
//! - `%<enum>.<variant>.payload` (per-variant payload): non-packed
//!   `struct_type(&fields, false)` over the variant's payload field
//!   types in declaration order. Skipped for Unit variants.
//!
//! Construction allocas the outer type (correct size + alignment),
//! then GEPs through the per-variant complete type for the tag and
//! the per-variant payload struct for fields. With opaque pointers
//! (`ptr`), the same alloca pointer flows through different GEPs
//! typed as different structs — no `bitcast` instruction is emitted
//! at the LLVM IR level.
//!
//! We diverge from v1's packed `{ i8, [N x i8] }` layout because
//! that layout misaligns payload fields (technically UB; relies on
//! x86_64 / ARM64 tolerating misaligned access). The Rust-style
//! layout is correct on every target LLVM supports.

use crate::function::IRSymbol;
use crate::struct_decl::{IRStructField, StructFieldInit};
use crate::types::{IRType, ValueId};

/// Discriminant tag for an enum variant. Wraps a `u8` because the
/// LLVM layout uses an `i8` field for the tag — keeps the tag width
/// contract on the type rather than scattered across call sites.
///
/// Mirrors the opaque-newtype pattern other IR identifiers use
/// ([`crate::IRBlockId`], [`crate::ValueId`], [`crate::IRLocalId`]):
/// distinct from raw `u8` so the type system distinguishes "this is
/// a variant tag" from "this is some other byte." `Display` renders
/// `#0`, `#1`, … to align with `bb<n>` / `%<n>` IR text-format
/// conventions.
///
/// **Transient invariant**: capped at 256 variants total per enum.
/// Lowering bounds-checks `position <= u8::MAX` and surfaces a
/// feature-gap diagnostic on overflow; the cap goes away when we
/// widen the tag (a follow-up beyond this slice).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IRVariantTag(pub u8);

impl std::fmt::Display for IRVariantTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A lowered enum declaration. `symbol` is the same package-qualified
/// mangled name shape an [`crate::IRFunction`] uses; `variants` is
/// the declaration-order variant list. Variant order *is* the tag
/// — variant `i` has `tag == IRVariantTag(i as u8)` — so seal
/// asserts dense, declaration-ordered tags. Generic decls never
/// appear here — [`crate::generics::instantiate`] produces one
/// [`IREnumDecl`] per discovered instantiation, keyed at its
/// mangled symbol.
#[derive(Debug, Clone)]
pub struct IREnumDecl {
    pub symbol: IRSymbol,
    pub variants: Vec<IREnumVariant>,
}

/// One variant of an [`IREnumDecl`]. `name` is the surface variant
/// name (`Some` in `Option.Some`); `payload` carries the variant's
/// data shape; `tag` is the discriminant byte (== position in
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
/// `Struct` arm reuses [`StructFieldInit`] — same canonicalization
/// invariant the struct slice already maintains: indices are
/// declaration-ordered with one entry per declared field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumPayloadInit {
    Struct(Vec<StructFieldInit>),
    Tuple(Vec<ValueId>),
    Unit,
}
