//! Per-`Expr` coercion annotations.
//!
//! Today's only family is **literal-fit width** — a numeric literal
//! expression flowing into a sized-numeric slot whose value fits the
//! slot's range. Typecheck stamps [`Expr::literal_coercion`] on the
//! literal (or the outer `Unary { Neg, .. }` for `-N`); the IR lowerer
//! reads the field and mints `Const u8 = 5` instead of the default
//! `Const i64 = 5`. There is no IR opcode — the coercion only changes
//! the leaf's materialized width.
//!
//! Future *value-conversion* coercions (fn-as-closure, `UnionWiden`,
//! `Display` in interpolation, list/map `from_list`, generic phi
//! widening) need a real `IRInstruction::*` because source type
//! != target type at runtime. Those will land as a dedicated
//! `ExprKind::Coercion { inner, kind: CoercionKind }` wrapper, not
//! as another arm of [`LiteralCoercion`].

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
