//! Per-`Expr` coercion annotations. Two parallel families — they
//! live on separate `Expr` slots because their downstream contracts
//! differ.
//!
//! **Literal-fit width** ([`LiteralCoercion`], stamped on
//! `Expr::literal_coercion`). A numeric literal flowing into a
//! sized-numeric slot whose value fits the slot's range. The IR
//! lowerer reads the field and mints `Const u8 = 5` instead of the
//! default `Const i64 = 5`. **No paired IR instruction** — the
//! annotation only changes which `Const` opcode is minted at the
//! literal leaf.
//!
//! **Value-conversion** ([`Coercion`], stamped on `Expr::coercion`).
//! A value of type `T` flowing into a slot of type `U ≠ T` where
//! the conversion needs runtime work — `UnionWiden` boxes a member
//! into a tagged union, `NumericWiden` extends a sized numeric
//! into its hub type (`Int` / `Float`); future variants will cover
//! fn-as-closure, `Display` in interpolation, list/map `from_list`,
//! generic phi widening. Per `COMPILER-NORTHSTAR.md`'s coercion
//! contract, every `Coercion::*` variant pairs 1:1 with an
//! `IRInstruction::*` variant that the lowerer emits at the exact
//! site (`Coercion::UnionWiden` ↔ `IRInstruction::UnionWrap`,
//! `Coercion::NumericWiden` ↔ `IRInstruction::NumericWiden`).
//! Adding a new `Coercion` variant requires adding the paired
//! `IRInstruction` and lowerer emitter in the same change.

use crate::identifier::ResolvedType;

/// Backend-stable target width for a coerced numeric literal.
/// Translated to the IR's typed `Const` opcode at lowering time
/// without crossing the typecheck → IR crate boundary on `IRType`
/// itself.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NumericLiteralWidth {
    Float32,
    Float64,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
}

impl NumericLiteralWidth {
    /// Short label used in diagnostics: `"Int8"`, `"UInt32"`, etc.
    pub fn label(self) -> &'static str {
        match self {
            Self::Float32 => "Float32",
            Self::Float64 => "Float64",
            Self::Int8 => "Int8",
            Self::Int16 => "Int16",
            Self::Int32 => "Int32",
            Self::Int64 => "Int64",
            Self::UInt8 => "UInt8",
            Self::UInt16 => "UInt16",
            Self::UInt32 => "UInt32",
            Self::UInt64 => "UInt64",
        }
    }

    /// Inclusive range rendered for out-of-range diagnostics. Floats
    /// label by representable shape rather than range bounds.
    pub fn range_label(self) -> &'static str {
        match self {
            Self::Float32 => "f32-representable values",
            Self::Float64 => "f64-representable values",
            Self::Int8 => "-128..=127",
            Self::Int16 => "-32_768..=32_767",
            Self::Int32 => "-2_147_483_648..=2_147_483_647",
            Self::Int64 => "-9_223_372_036_854_775_808..=9_223_372_036_854_775_807",
            Self::UInt8 => "0..=255",
            Self::UInt16 => "0..=65_535",
            Self::UInt32 => "0..=4_294_967_295",
            Self::UInt64 => "0..=18_446_744_073_709_551_615",
        }
    }
}

/// Per-expression coercion annotation. One variant today (numeric
/// literal width); see the module docs for why value-conversion
/// coercions don't share this enum.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LiteralCoercion {
    /// The expression is a numeric literal (or `-literal` unary)
    /// whose materialized `Const` should be minted at this width
    /// instead of the literal's default `Int` (i64) / `Float` (f64).
    NumericLiteralWidth(NumericLiteralWidth),
}

impl LiteralCoercion {
    /// Convenience: pull out the numeric width if this annotation is
    /// the literal-width variant. Reserved for the day a second
    /// variant lands.
    pub fn numeric_width(&self) -> Option<NumericLiteralWidth> {
        let Self::NumericLiteralWidth(width) = self;
        Some(*width)
    }
}

/// Per-expression value-conversion coercion. Each variant pairs
/// 1:1 with an `IRInstruction::*` variant that the IR lowerer
/// emits at the annotated site. See module doc for the full design
/// contract (annotation vs literal-fit width).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Coercion {
    /// A sized numeric value flowing into its hub-type slot: any of
    /// `Int8` / `Int16` / `Int32` / `UInt8` / `UInt16` / `UInt32`
    /// into `Int`, or `Float32` into `Float`. Hub-only and lossless
    /// by construction — sideways widening (`Int8 -> Int16`) and
    /// `UInt64 -> Int` are rejected at typecheck. The carried
    /// `ResolvedType` is the target as declared at the slot. The
    /// source width and signedness come from the annotated
    /// expression's own resolution. Lowers to
    /// `IRInstruction::NumericWiden` (sign-extend signed sources,
    /// zero-extend unsigned, `fpext` for `Float32`).
    NumericWiden(ResolvedType),
    /// Member `M` flowing into union slot `M | ...`. The carried
    /// `ResolvedType` is the *target union as declared at the slot*,
    /// preserved verbatim so an alias-named target keeps its name
    /// in diagnostics and the IR lowerer can peel it once when
    /// shaping the `UnionWrap`. Lowers to `IRInstruction::UnionWrap`.
    UnionWiden(ResolvedType),
}
