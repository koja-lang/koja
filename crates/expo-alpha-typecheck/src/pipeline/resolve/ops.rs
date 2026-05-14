//! Type rules for literal / binary / unary expressions.
//!
//! Every helper is registry-backed: outputs flow through
//! [`GlobalRegistry::primitive`] so primitive identity stays
//! single-sourced. On a type mismatch we emit a diagnostic and return
//! [`ResolvedType::unresolved`]; resolve never aborts mid-walk, so a
//! follow-on type rule sees `<unresolved>` operands and stays quiet
//! ([`super::types::is_primitive`] short-circuits on those).
//!
//! Numeric arms (arithmetic + comparison) accept any two operands
//! [`super::types::types_equivalent`] considers compatible. Today
//! that's `Int ≡ Int64` and `Float ≡ Float64` — the alias rule that
//! stands in for future union-membership: `Int` is on track to
//! become a `Int8 | Int16 | Int32 | Int64` union with `Int64` as
//! one of its members, at which point this same predicate keeps
//! working with no per-call-site changes.
//!
//! Comparison arms additionally reuse [`super::coercion::check_compatible`]
//! so a default `Int` / `Float` literal paired with a sized-numeric
//! operand (`Int32 == 0`, `fd: Int32 >= 0`) picks up the matching
//! [`LiteralCoercion`] — same plumbing the four existing coercion
//! sites use, just invoked at one more site.

use expo_ast::ast::{BinOp, Diagnostic, Expr, UnaryOp};
use expo_ast::coercion::LiteralCoercion;
use expo_ast::identifier::ResolvedType;
use expo_ast::labels::bin_op_label;
use expo_ast::span::Span;

use super::coercion::{Compatible, check_compatible, coercion_target_mut};
use super::types::{display_resolution, is_primitive, types_equivalent};
use crate::registry::GlobalRegistry;

pub(super) fn binary_type(
    op: BinOp,
    left: &mut Expr,
    right: &mut Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match op {
        BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
            // Arithmetic preserves the operand's numeric width: both
            // sides must agree on `Int` or `Float`. Cross-numeric
            // mixing (`1 + 1.0`) and sized-numeric arithmetic
            // (`Int8 + Int8`) are deferred per `LANGUAGE.md` /
            // `V1-PARITY.md`. The alias rule still applies, so
            // `Int + Int64` is valid and produces `Int` — same
            // predicate that will accept `Int8 + Int8` once `Int`
            // becomes a real union over its sized variants.
            if both_aliased(&left.resolution, &right.resolution, registry, "Int") {
                registry.primitive("Int")
            } else if both_aliased(&left.resolution, &right.resolution, registry, "Float") {
                registry.primitive("Float")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "Int or Float operands of the same type",
                    &left.resolution,
                    &right.resolution,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::And | BinOp::Or => {
            if both(&left.resolution, &right.resolution, registry, "Bool") {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "Bool operands",
                    &left.resolution,
                    &right.resolution,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::Eq | BinOp::NotEq => {
            // String operands flow through the same operator so
            // `match` arms with string-literal patterns can desugar
            // to the same equality chain; LLVM emit routes the
            // `IRType::String` case through `strcmp`. The numeric
            // path covers both `Int ≡ Int64` / `Float ≡ Float64`
            // alias mixes and sized-numeric vs default-literal pairs
            // (`Int32 == 0`, `fd >= 0`) — the latter stamps the
            // matching [`LiteralCoercion`] on the literal side.
            let bool_match = both(&left.resolution, &right.resolution, registry, "Bool");
            let string_match = both(&left.resolution, &right.resolution, registry, "String");
            if bool_match || string_match || numeric_comparison_compatible(left, right, registry) {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "matching Bool, Float, Int, or String operands",
                    &left.resolution,
                    &right.resolution,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::Gt | BinOp::GtEq | BinOp::Lt | BinOp::LtEq => {
            if numeric_comparison_compatible(left, right, registry) {
                registry.primitive("Bool")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "Int or Float operands of the same type",
                    &left.resolution,
                    &right.resolution,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
        BinOp::Concat => {
            // `<>` requires both operands to share a heap-payload
            // type — `String`, `Binary`, or `Bits`. Cross-type
            // concat (e.g. `String <> Binary`) is rejected; the
            // user must convert through a stdlib helper. Result
            // type matches operands.
            if both(&left.resolution, &right.resolution, registry, "String") {
                registry.primitive("String")
            } else if both(&left.resolution, &right.resolution, registry, "Binary") {
                registry.primitive("Binary")
            } else if both(&left.resolution, &right.resolution, registry, "Bits") {
                registry.primitive("Bits")
            } else {
                push_op_mismatch(
                    diagnostics,
                    op,
                    "matching String, Binary, or Bits operands",
                    &left.resolution,
                    &right.resolution,
                    span,
                    registry,
                );
                ResolvedType::unresolved()
            }
        }
    }
}

pub(super) fn unary_type(
    op: UnaryOp,
    operand: &Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = &operand.resolution;
    match op {
        UnaryOp::Neg => {
            if is_primitive(ty, registry, "Int") {
                registry.primitive("Int")
            } else if is_primitive(ty, registry, "Float") {
                registry.primitive("Float")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "unary `-` requires an Int or Float operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        UnaryOp::Not => {
            if is_primitive(ty, registry, "Bool") {
                registry.primitive("Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`not` requires a Bool operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
    }
}

fn both(lhs: &ResolvedType, rhs: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    is_primitive(lhs, registry, name) && is_primitive(rhs, registry, name)
}

/// Compatibility-aware variant of [`both`] for numeric primitives.
/// Returns true when both operands are members of the same numeric
/// union per [`types_equivalent`]. Today the only such unions are
/// the alias pairs `Int = {Int, Int64}` and `Float = {Float, Float64}`;
/// when `Int` becomes a real union over `Int8 | Int16 | Int32 | Int64`
/// (see `LANGUAGE.md` primitives table) this same predicate keeps
/// working — the membership check generalizes inside `types_equivalent`,
/// not here.
///
/// Sized numeric primitives (`Int8` … `UInt64`, `Float32`) are not
/// members of `Int` / `Float` today, so they flow through
/// [`numeric_comparison_compatible`] at comparison sites via the
/// literal-coercion path. Arithmetic against them is deferred per
/// `V1-PARITY.md`.
fn both_aliased(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    registry: &GlobalRegistry,
    name: &str,
) -> bool {
    let canonical = registry.primitive(name);
    types_equivalent(lhs, &canonical, registry) && types_equivalent(rhs, &canonical, registry)
}

/// Equality / comparison numeric-operand rule. Accepts:
///
/// - Both operands alias-equivalent to `Int` (covers `Int ↔ Int64`
///   mixes — the typical FFI-result-vs-literal case).
/// - Both operands alias-equivalent to `Float` (analogous).
/// - Both operands are the SAME sized-numeric primitive (`UInt8 ==
///   UInt8`, `Int32 < Int32`, etc). Same-type sized comparison is a
///   narrow allowance that lets stdlib byte-walking / FD-handle
///   code compare values without round-tripping through `Int`. The
///   broader `IntLiteral`-protocol story (cross-width arithmetic,
///   mixed sized + default-literal arithmetic) stays deferred.
/// - One sized-numeric operand (`Int8` … `UInt64`, `Float32`) paired
///   with a default `Int` / `Float` literal whose value fits the
///   sized type's range. The literal AST node is stamped with the
///   matching [`LiteralCoercion`] via [`coercion_target_mut`] — same
///   plumbing struct-field / call-arg / return / enum-payload /
///   const-init sites use, just invoked at one more site.
///
/// Rejects everything else (`Bool` / `String` operands have their
/// own arms in [`binary_type`]; user types fall through to the
/// type-mismatch diagnostic).
fn numeric_comparison_compatible(
    left: &mut Expr,
    right: &mut Expr,
    registry: &GlobalRegistry,
) -> bool {
    if both_aliased(&left.resolution, &right.resolution, registry, "Int")
        || both_aliased(&left.resolution, &right.resolution, registry, "Float")
        || both_same_sized_numeric(&left.resolution, &right.resolution, registry)
    {
        return true;
    }
    let lhs = left.resolution.clone();
    let rhs = right.resolution.clone();
    coerce_literal_to(left, &rhs, registry) || coerce_literal_to(right, &lhs, registry)
}

/// True when `lhs` and `rhs` resolve to the same sized-numeric
/// primitive. Independent of the alias rule (`Int ≡ Int64`) so
/// `Int64 == Int64` still flows through the alias path, while
/// `UInt8 == UInt8` / `Int32 == Int32` / `Float32 == Float32`
/// pick up here. Sized-vs-default-literal mixes (`UInt8 == 0`)
/// continue to take the literal-coercion branch in the caller.
fn both_same_sized_numeric(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    const SIZED_NUMERIC: &[&str] = &[
        "Float32", "Int16", "Int32", "Int64", "Int8", "UInt16", "UInt32", "UInt64", "UInt8",
    ];
    SIZED_NUMERIC
        .iter()
        .any(|name| is_primitive(lhs, registry, name) && is_primitive(rhs, registry, name))
}

/// Try to stamp a literal-width [`LiteralCoercion`] on `actual` so
/// it flows into the sized `target_ty` slot. Returns `true` on
/// successful coercion; `false` for non-sized targets, non-literal
/// sources, or out-of-range values — comparison sites fall back to
/// the type-mismatch diagnostic in those cases. Out-of-range
/// diagnostics are deferred to the caller's mismatch path: a
/// dedicated narrow-int diagnostic at binary-op sites would conflate
/// "operand types disagree" with "literal value too wide", and the
/// existing four coercion sites already surface the latter at slot
/// boundaries before a comparison site sees it.
fn coerce_literal_to(
    actual: &mut Expr,
    target_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    let actual_ty = actual.resolution.clone();
    let Compatible::Coerced(width) = check_compatible(actual, &actual_ty, target_ty, registry)
    else {
        return false;
    };
    *coercion_target_mut(actual) = Some(LiteralCoercion::NumericLiteralWidth(width));
    true
}

fn push_op_mismatch(
    diagnostics: &mut Vec<Diagnostic>,
    op: BinOp,
    expected: &str,
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "`{}` requires {expected}; got `{}` and `{}`",
            bin_op_label(op),
            display_resolution(lhs, registry),
            display_resolution(rhs, registry),
        ),
        span,
    ));
}
