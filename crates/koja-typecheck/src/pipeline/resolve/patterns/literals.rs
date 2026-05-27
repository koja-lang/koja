//! Literal-pattern helpers: subject-vs-literal type checking and
//! the canonical literal-string representation used by
//! [`super::collect_literal_reprs`] and by [`super::or_pattern`]
//! for intra-or-pattern overlap detection.

use koja_ast::ast::{Diagnostic, Literal};
use koja_ast::coercion::LiteralCoercion;
use koja_ast::identifier::ResolvedType;
use koja_ast::span::Span;

use super::super::coercion::{
    float_value_fits, int_value_fits, narrow_numeric_target, parse_int_literal_text,
};
use super::super::ctx::Resolver;
use super::super::types::{display_resolution, is_primitive};

/// Check that a `Pattern::Literal`'s value agrees with the subject
/// type. Strict equality on the literal's default head, with one
/// allowance: if the subject is a sized numeric primitive and the
/// pattern's literal value fits the subject's range, stamp the
/// pattern's `literal_coercion` so IR-side equality lowering mints
/// a matching narrow `Const`. Out-of-range literals diagnose with
/// the same `OutOfRange` shape used at expression-coercion sites.
pub(super) fn check_literal_matches_subject(
    value: &Literal,
    coercion_slot: &mut Option<LiteralCoercion>,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !subject_ty.is_resolved() {
        return;
    }
    let lit_ty = resolver.registry.literal_type(value);
    if &lit_ty == subject_ty {
        return;
    }
    if let Some(width) = narrow_numeric_target(subject_ty, resolver.registry) {
        if is_primitive(&lit_ty, resolver.registry, "Int")
            && let Literal::Int(text) = value
            && let Some(int_value) = parse_int_literal_text(text)
        {
            if int_value_fits(int_value, width) {
                *coercion_slot = Some(LiteralCoercion::NumericLiteralWidth(width));
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "match literal `{int_value}` does not fit subject type `{}` \
                         (range {})",
                        width.label(),
                        width.range_label(),
                    ),
                    span,
                ));
            }
            return;
        }
        if is_primitive(&lit_ty, resolver.registry, "Float")
            && let Literal::Float(text) = value
            && let Ok(float_value) = text.parse::<f64>()
        {
            if float_value_fits(float_value, width) {
                *coercion_slot = Some(LiteralCoercion::NumericLiteralWidth(width));
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "match literal `{float_value}` does not fit subject type `{}` \
                         (range {})",
                        width.label(),
                        width.range_label(),
                    ),
                    span,
                ));
            }
            return;
        }
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "match literal pattern of type `{}` does not match subject type `{}`",
            display_resolution(&lit_ty, resolver.registry),
            display_resolution(subject_ty, resolver.registry),
        ),
        span,
    ));
}

/// Canonical surface-string form of a literal pattern's value.
/// Stable enough to use as a dedupe key for cross-arm literal-arm
/// reachability checks. Strings are wrapped in quotes so an `Int`
/// literal "true" never collides with a `Bool` literal `true`
/// (subjects in the same `match` always have the same type, so the
/// collision is theoretical, but the wrapping costs nothing).
pub(super) fn literal_repr(value: &Literal) -> String {
    match value {
        Literal::Bool(b) => b.to_string(),
        Literal::Float(s) => s.clone(),
        Literal::Int(s) => s.clone(),
        Literal::String(s) => format!("\"{s}\""),
        Literal::Unit => "()".to_string(),
    }
}
