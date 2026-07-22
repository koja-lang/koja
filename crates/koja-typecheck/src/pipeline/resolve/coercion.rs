//! Numeric literal-fit checking shared across every type-equality
//! site (struct fields, the three call-arg flavors, return type,
//! enum tuple payloads, const initializers). Each site asks
//! [`check_compatible`] whether the actual expression's resolved
//! type flows into the declared slot, and on a [`Compatible::Coerced`]
//! result stamps `expr.literal_coercion` via [`coercion_target_mut`]
//! so `koja-ir`'s lowerer mints the matching narrow `Const`
//! opcode.
//!
//! The rule: a numeric literal coerces to a sized target type iff
//! its compile-time value fits the target's range. `Int` ≡ `Int64`
//! and `Float` ≡ `Float64` are still handled by
//! [`super::types::types_equivalent`] (an alias hit returns
//! [`Compatible::Strict`] before the literal-fit path runs), so only
//! the narrower / unsigned widths exercise this module.
//!
//! Non-literal sized numeric *values* get the inverse treatment:
//! hub-only implicit widening ([`widens_to_hub`]). Any of `Int8` /
//! `Int16` / `Int32` / `UInt8` / `UInt16` / `UInt32` flows into an
//! `Int` slot and `Float32` flows into a `Float` slot, stamping
//! `expr.coercion = Coercion::NumericWiden` so IR lowering emits
//! the extension. Sideways widening between sized types is
//! rejected by design.
//!
//! Negated integer literals (`-128`) are recognized via a
//! `UnaryOp::Neg(Literal::Int)` shape and folded to a single
//! `i128` value at typecheck so the same fit-check covers both
//! positive and negative literals uniformly. Hex / binary literals
//! (`0xFF`, `0b1010`) parse to positive integers, the bit-pattern
//! escape hatch for unsigned targets where `-1: UInt8` is rejected.

use koja_ast::ast::{Diagnostic, Expr, ExprKind, Literal, UnaryOp};
use koja_ast::coercion::{Coercion, LiteralCoercion, NumericLiteralWidth};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_ast::span::Span;

use super::types::{is_primitive, peel_alias, types_equivalent};
use crate::registry::GlobalRegistry;

/// Reject a float literal whose decimal text rounds to infinity
/// under `f64`, upholding the finite-only `Float` invariant at the
/// source. Same `OutOfRange` classification as `Float.parse`.
pub(crate) fn check_float_literal_finite(
    value: &Literal,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Literal::Float(text) = value else { return };
    if text.parse::<f64>().is_ok_and(f64::is_finite) {
        return;
    }
    diagnostics.push(Diagnostic::error(
        "float literal is out of range (magnitude too large for a 64-bit `Float`)".to_string(),
        span,
    ));
}

/// Outcome of comparing an actual expression's resolved type
/// against an expected type with coercion considered. The arms map
/// 1:1 to caller behavior at each check site: `Strict` proceeds,
/// `Coerced` stamps `expr.literal_coercion` via
/// [`coercion_target_mut`] (so IR-lower picks up the width),
/// `NumericWiden` / `UnionWiden` stamp `expr.coercion` via
/// [`coercion_annotation_mut`], `OutOfRange` emits a precise
/// narrow-int diagnostic, and `Incompatible` falls through to the
/// existing type-mismatch diagnostic. Check sites share the arm
/// handling through [`check_compatible_stamping`].
#[derive(Debug)]
pub(crate) enum Compatible {
    /// `types_equivalent` already accepts the pair, no coercion
    /// needed.
    Strict,
    /// The actual expression is a numeric literal whose value fits
    /// the expected type's range. Caller stamps the AST node via
    /// [`coercion_target_mut`] and proceeds.
    Coerced(NumericLiteralWidth),
    /// The actual expression is a sized numeric value flowing into
    /// its hub type: any of `Int8` / `Int16` / `Int32` / `UInt8` /
    /// `UInt16` / `UInt32` into `Int`, or `Float32` into `Float`.
    /// Caller stamps `expr.coercion =
    /// Some(Coercion::NumericWiden(target))` so IR lowering emits a
    /// `NumericWiden`. `target` is the expected type as declared at
    /// the slot, preserved verbatim for diagnostics.
    NumericWiden { target: ResolvedType },
    /// The actual expression's type is one member of the expected
    /// union. Caller stamps `expr.coercion =
    /// Some(Coercion::UnionWiden(target))` so IR lowering emits a
    /// `UnionWrap` against the target union shape. `target` is the
    /// (possibly aliased) union expected type as declared at the
    /// slot, preserved verbatim so diagnostics and downstream IR
    /// see the user's name when an alias was used.
    UnionWiden { target: ResolvedType },
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

/// The rejecting subset of [`Compatible`], returned by
/// [`check_compatible_stamping`] so each check site renders its own
/// site-specific diagnostic while the accepting arms (and their
/// AST stamping) stay shared.
pub(crate) enum Mismatch {
    /// Numeric literal whose value does not fit the expected range.
    OutOfRange {
        rendered_value: String,
        width: NumericLiteralWidth,
    },
    /// Types are incompatible regardless of literal context.
    Incompatible,
}

enum TupleCompatibility {
    Compatible,
    Mismatch(Mismatch),
    NotTupleLiteral,
}

/// Run [`check_compatible`] for `expr` (whose resolved type is
/// `actual_ty`) flowing into `expected_ty`, stamping the matching
/// coercion slot on the expression for every accepting arm.
/// Returns `None` when the flow is accepted, or the [`Mismatch`]
/// for the caller to diagnose.
pub(crate) fn check_compatible_stamping(
    expr: &mut Expr,
    actual_ty: &ResolvedType,
    expected_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Option<Mismatch> {
    match check_compatible(expr, actual_ty, expected_ty, registry) {
        Compatible::Strict => None,
        Compatible::Coerced(width) => {
            *coercion_target_mut(expr) = Some(LiteralCoercion::NumericLiteralWidth(width));
            None
        }
        Compatible::NumericWiden { target } => {
            *coercion_annotation_mut(expr) = Some(Coercion::NumericWiden(target));
            None
        }
        Compatible::UnionWiden { target } => {
            *coercion_annotation_mut(expr) = Some(Coercion::UnionWiden(target));
            None
        }
        Compatible::OutOfRange {
            rendered_value,
            width,
        } => Some(Mismatch::OutOfRange {
            rendered_value,
            width,
        }),
        Compatible::Incompatible => {
            match check_tuple_literal_compatible(expr, actual_ty, expected_ty, registry) {
                TupleCompatibility::Compatible => None,
                TupleCompatibility::Mismatch(mismatch) => Some(mismatch),
                TupleCompatibility::NotTupleLiteral => Some(Mismatch::Incompatible),
            }
        }
    }
}

fn check_tuple_literal_compatible(
    expr: &mut Expr,
    actual_ty: &ResolvedType,
    expected_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> TupleCompatibility {
    let actual = peel_alias(actual_ty, registry);
    let expected = peel_alias(expected_ty, registry);
    let (
        ResolvedType::Anonymous(AnonymousKind::Tuple {
            elements: actual_types,
        }),
        ResolvedType::Anonymous(AnonymousKind::Tuple {
            elements: expected_types,
        }),
    ) = (actual, expected)
    else {
        return TupleCompatibility::NotTupleLiteral;
    };
    let Some(elements) = tuple_literal_elements_mut(expr) else {
        return TupleCompatibility::NotTupleLiteral;
    };
    if elements.len() != actual_types.len() || elements.len() != expected_types.len() {
        return TupleCompatibility::Mismatch(Mismatch::Incompatible);
    }
    for ((element, actual), expected) in elements.iter_mut().zip(&actual_types).zip(&expected_types)
    {
        if let Some(mismatch) = check_compatible_stamping(element, actual, expected, registry) {
            return TupleCompatibility::Mismatch(mismatch);
        }
    }
    TupleCompatibility::Compatible
}

fn tuple_literal_elements_mut(expr: &mut Expr) -> Option<&mut [Expr]> {
    match &mut expr.kind {
        ExprKind::Group { expr: inner } => tuple_literal_elements_mut(inner),
        ExprKind::Tuple { elements } => Some(elements),
        _ => None,
    }
}

/// Mutable handle to the AST node that owns the coercion annotation
/// for `expr`. Peels through [`ExprKind::Group`] so a coercion
/// recorded on `(1)` lands on the inner literal where the IR
/// lowerer will read it. Bare literals stamp on themselves, and
/// `Unary { Neg, .. }` stamps on the outer unary so the negated-
/// literal fold finds it on the materialized expression.
pub(crate) fn coercion_target_mut(expr: &mut Expr) -> &mut Option<LiteralCoercion> {
    match &mut expr.kind {
        ExprKind::Group { expr: inner } => coercion_target_mut(inner),
        _ => &mut expr.literal_coercion,
    }
}

/// Mutable handle to the AST node that owns the value-conversion
/// [`Coercion`] annotation for `expr`. Same `Group` peel as
/// [`coercion_target_mut`] so a coercion recorded on `(x)` lands on
/// the inner expression that IR lowering actually emits a value
/// for.
pub(crate) fn coercion_annotation_mut(expr: &mut Expr) -> &mut Option<Coercion> {
    match &mut expr.kind {
        ExprKind::Group { expr: inner } => coercion_annotation_mut(inner),
        _ => &mut expr.coercion,
    }
}

/// Recognize an integer literal expression (bare or
/// `UnaryOp::Neg`-wrapped) and return its compile-time value as
/// `i128`. Returns `None` for any other shape, so callers can
/// distinguish "non-literal source" from "out-of-range literal."
/// Hex (`0xFF`) and binary (`0b1010`) literals parse to positive
/// integers. The only path to a negative value is the
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
/// (typically `expr.resolution`), and the `expr` argument is the AST
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
    if widens_to_hub(actual_ty, expected_ty, registry) {
        return Compatible::NumericWiden {
            target: expected_ty.clone(),
        };
    }
    if let ResolvedType::Union(members) = peel_alias(expected_ty, registry)
        && members
            .iter()
            .any(|m| types_equivalent(actual_ty, m, registry))
    {
        return Compatible::UnionWiden {
            target: expected_ty.clone(),
        };
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

/// Hub-only lossless numeric widening: a sized integer value flows
/// into an `Int` slot (`Int8` / `Int16` / `Int32` sign-extend,
/// `UInt8` / `UInt16` / `UInt32` zero-extend), and `Float32` flows
/// into a `Float` slot. Sideways widening (`Int8 -> Int16`) and
/// `UInt64 -> Int` (doesn't fit) are deliberately excluded: every
/// source type has exactly one implicit target, so a future
/// overload-resolution rule only ever needs "exact match beats
/// widened match."
fn widens_to_hub(
    actual_ty: &ResolvedType,
    expected_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    const INT_SOURCES: &[&str] = &["Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32"];
    let expected = peel_alias(expected_ty, registry);
    if is_primitive(&expected, registry, "Int") || is_primitive(&expected, registry, "Int64") {
        return INT_SOURCES
            .iter()
            .any(|name| is_primitive(actual_ty, registry, name));
    }
    if is_primitive(&expected, registry, "Float") || is_primitive(&expected, registry, "Float64") {
        return is_primitive(actual_ty, registry, "Float32");
    }
    false
}

/// Parse a numeric literal's source text (decimal, hex (`0xFF`),
/// or binary (`0b1010`), with `_` separators stripped) into a
/// signed `i128`. Returns `None` on overflow or malformed input.
pub(crate) fn parse_int_literal_text(text: &str) -> Option<i128> {
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
