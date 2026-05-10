//! Span-keyed numeric literal coercion table. Populated alongside
//! `types_equivalent` checks at the six type-equality sites
//! (struct fields, three call-arg flavors, return type, enum tuple
//! payloads) plus the const-initializer pass; consumed by
//! `expo-alpha-ir`'s expression lowerer at [`Literal`] and
//! [`UnaryOp::Neg(Literal)`] sites to mint the `Const` instruction
//! at the recorded width.
//!
//! The rule: a numeric literal coerces to a sized target type iff
//! its compile-time value fits the target's range. `Int` ≡ `Int64`
//! and `Float` ≡ `Float64` are still handled by
//! [`super::types::types_equivalent`] (an alias hit returns
//! [`Compatible::Strict`] before the literal-fit path runs); only
//! the narrower / unsigned widths exercise this module.
//!
//! Negated integer literals (`-128`) are recognized via a
//! `UnaryOp::Neg(Literal::Int)` shape and folded to a single
//! `i128` value at typecheck so the same fit-check covers both
//! positive and negative literals uniformly. Hex / binary literals
//! (`0xFF`, `0b1010`) parse to positive integers — the bit-pattern
//! escape hatch for unsigned targets where `-1: UInt8` is rejected.
//!
//! [`Literal`]: expo_ast::ast::Literal
//! [`UnaryOp::Neg(Literal)`]: expo_ast::ast::UnaryOp::Neg

use std::collections::HashMap;

use expo_ast::ast::{Expr, ExprKind, Literal, UnaryOp};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::types::{is_primitive, types_equivalent};
use crate::registry::GlobalRegistry;

/// Backend-stable target width for a coerced numeric literal.
/// Translated to `expo_alpha_ir::IRType` at lowering time without
/// crossing the typecheck → IR crate boundary on `IRType` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Span-keyed map of expression spans to the IR-time numeric width
/// the lowerer should mint for them. Spans key the *outer*
/// expression the lowerer materializes — for negated literals
/// (`UnaryOp::Neg(Literal::Int)`), the `Unary`'s span, which the
/// lowerer reads to fold the negation into a single `Const`. For
/// bare literals (or hex / binary literals), the literal's own
/// span. `Group { expr: inner }` peels are handled at recording
/// time so the recorded span lands on the materialized expression.
pub type Coercions = HashMap<Span, NumericLiteralWidth>;

/// Outcome of comparing an actual expression's resolved type
/// against an expected type with literal-fit coercion considered.
/// The four arms map 1:1 to caller behavior at each check site:
/// `Strict` proceeds; `Coerced` records into the coercion table;
/// `OutOfRange` emits a precise narrow-int diagnostic;
/// `Incompatible` falls through to the existing type-mismatch
/// diagnostic.
#[derive(Debug)]
pub(crate) enum Compatible {
    /// `types_equivalent` already accepts the pair — no coercion
    /// needed.
    Strict,
    /// The actual expression is a numeric literal whose value fits
    /// the expected type's range. Caller records the span (via
    /// [`coercion_span`]) in the coercion table and proceeds.
    Coerced(NumericLiteralWidth),
    /// The actual expression is a numeric literal whose value does
    /// NOT fit the expected type's range. Caller emits a precise
    /// out-of-range diagnostic.
    OutOfRange {
        rendered_value: String,
        width: NumericLiteralWidth,
    },
    /// Types are incompatible regardless of literal context.
    /// Caller emits the existing type-mismatch diagnostic.
    Incompatible,
}

/// Pick the span the lowerer will see when materializing this
/// expression — peels through [`ExprKind::Group`] so a coercion
/// recorded on `(1)` lands on the inner literal where the lowerer
/// will read it.
pub fn coercion_span(expr: &Expr) -> Span {
    match &expr.kind {
        ExprKind::Group { expr: inner } => coercion_span(inner),
        _ => expr.span,
    }
}

/// Recognize an integer literal expression — bare or
/// `UnaryOp::Neg`-wrapped — and return its compile-time value as
/// `i128`. Returns `None` for any other shape, so callers can
/// distinguish "non-literal source" from "out-of-range literal."
/// Hex (`0xFF`) and binary (`0b1010`) literals parse to positive
/// integers; the only path to a negative value is the
/// `UnaryOp::Neg(Literal::Int(decimal))` shape.
pub fn evaluate_int_literal(expr: &Expr) -> Option<i128> {
    match &expr.kind {
        ExprKind::Group { expr: inner } => evaluate_int_literal(inner),
        ExprKind::Literal {
            value: Literal::Int(text),
        } => parse_int_literal_text(text),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            let inner_value = match &peel_groups(operand).kind {
                ExprKind::Literal {
                    value: Literal::Int(text),
                } => parse_int_literal_text(text)?,
                _ => return None,
            };
            inner_value.checked_neg()
        }
        _ => None,
    }
}

/// Recognize a float literal expression, including the
/// `UnaryOp::Neg`-wrapped form. Returns the parsed `f64` value.
pub fn evaluate_float_literal(expr: &Expr) -> Option<f64> {
    match &expr.kind {
        ExprKind::Group { expr: inner } => evaluate_float_literal(inner),
        ExprKind::Literal {
            value: Literal::Float(text),
        } => text.parse::<f64>().ok(),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match &peel_groups(operand).kind {
            ExprKind::Literal {
                value: Literal::Float(text),
            } => text.parse::<f64>().ok().map(|v| -v),
            _ => None,
        },
        _ => None,
    }
}

fn peel_groups(expr: &Expr) -> &Expr {
    match &expr.kind {
        ExprKind::Group { expr: inner } => peel_groups(inner),
        _ => expr,
    }
}

/// Range check for `value` against `width`. Float widths return
/// `false` (callers are responsible for using
/// [`float_value_fits`]).
pub fn int_value_fits(value: i128, width: NumericLiteralWidth) -> bool {
    match width {
        NumericLiteralWidth::Int8 => (i8::MIN as i128..=i8::MAX as i128).contains(&value),
        NumericLiteralWidth::Int16 => (i16::MIN as i128..=i16::MAX as i128).contains(&value),
        NumericLiteralWidth::Int32 => (i32::MIN as i128..=i32::MAX as i128).contains(&value),
        NumericLiteralWidth::Int64 => (i64::MIN as i128..=i64::MAX as i128).contains(&value),
        NumericLiteralWidth::UInt8 => (0..=u8::MAX as i128).contains(&value),
        NumericLiteralWidth::UInt16 => (0..=u16::MAX as i128).contains(&value),
        NumericLiteralWidth::UInt32 => (0..=u32::MAX as i128).contains(&value),
        NumericLiteralWidth::UInt64 => (0..=u64::MAX as i128).contains(&value),
        NumericLiteralWidth::Float32 | NumericLiteralWidth::Float64 => false,
    }
}

/// Round-trip representability check: a literal that lexically
/// parses as `f64` fits `Float32` iff it round-trips equal
/// through `f64 -> f32 -> f64`. `Float64` always fits the source
/// since the lexer parses every float literal as `f64`. Int
/// widths return `false` (callers use [`int_value_fits`]).
pub fn float_value_fits(value: f64, width: NumericLiteralWidth) -> bool {
    match width {
        NumericLiteralWidth::Float32 => (value as f32) as f64 == value,
        NumericLiteralWidth::Float64 => true,
        _ => false,
    }
}

/// Map a [`ResolvedType`] head onto a [`NumericLiteralWidth`] when
/// it's one of the sized numeric primitives. Returns `None` for
/// the `Int` / `Float` aliases (those are handled by the strict
/// `types_equivalent` arm before this module runs) and for any
/// non-primitive type.
pub fn narrow_numeric_target(
    ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Option<NumericLiteralWidth> {
    if is_primitive(ty, registry, "Int8") {
        Some(NumericLiteralWidth::Int8)
    } else if is_primitive(ty, registry, "Int16") {
        Some(NumericLiteralWidth::Int16)
    } else if is_primitive(ty, registry, "Int32") {
        Some(NumericLiteralWidth::Int32)
    } else if is_primitive(ty, registry, "Int64") {
        Some(NumericLiteralWidth::Int64)
    } else if is_primitive(ty, registry, "UInt8") {
        Some(NumericLiteralWidth::UInt8)
    } else if is_primitive(ty, registry, "UInt16") {
        Some(NumericLiteralWidth::UInt16)
    } else if is_primitive(ty, registry, "UInt32") {
        Some(NumericLiteralWidth::UInt32)
    } else if is_primitive(ty, registry, "UInt64") {
        Some(NumericLiteralWidth::UInt64)
    } else if is_primitive(ty, registry, "Float32") {
        Some(NumericLiteralWidth::Float32)
    } else if is_primitive(ty, registry, "Float64") {
        Some(NumericLiteralWidth::Float64)
    } else {
        None
    }
}

/// Decide compatibility of an actual expression flowing into a
/// slot whose declared type is `expected_ty`. The `actual_ty`
/// argument is the resolved type of the source expression
/// (typically `expr.resolution`); the `expr` argument is the AST
/// node so the literal-shape inspection can read the unwrapped
/// integer / float value.
pub(crate) fn check_compatible(
    actual_expr: &Expr,
    actual_ty: &ResolvedType,
    expected_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Compatible {
    if types_equivalent(actual_ty, expected_ty, registry) {
        return Compatible::Strict;
    }
    let Some(target_width) = narrow_numeric_target(expected_ty, registry) else {
        return Compatible::Incompatible;
    };
    if is_primitive(actual_ty, registry, "Int") {
        let Some(value) = evaluate_int_literal(actual_expr) else {
            return Compatible::Incompatible;
        };
        if int_value_fits(value, target_width) {
            return Compatible::Coerced(target_width);
        }
        return Compatible::OutOfRange {
            rendered_value: format!("{value}"),
            width: target_width,
        };
    }
    if is_primitive(actual_ty, registry, "Float") {
        let Some(value) = evaluate_float_literal(actual_expr) else {
            return Compatible::Incompatible;
        };
        if float_value_fits(value, target_width) {
            return Compatible::Coerced(target_width);
        }
        return Compatible::OutOfRange {
            rendered_value: format!("{value}"),
            width: target_width,
        };
    }
    Compatible::Incompatible
}

fn parse_int_literal_text(text: &str) -> Option<i128> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        i128::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        i128::from_str_radix(bin, 2).ok()
    } else {
        cleaned.parse::<i128>().ok()
    }
}
