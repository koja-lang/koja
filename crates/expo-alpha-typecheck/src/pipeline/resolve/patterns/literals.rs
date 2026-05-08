//! Literal-pattern helpers: subject-vs-literal type checking and
//! the canonical literal-string representation used by
//! [`super::collect_literal_reprs`] and by [`super::or_pattern`]
//! for intra-or-pattern overlap detection.

use expo_ast::ast::{Diagnostic, Literal};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::ops::literal_type;
use super::super::types::display_resolution;

/// Diagnose when a `Pattern::Literal`'s value doesn't agree with
/// the subject type. No coercion — strict equality, matching the
/// rest of alpha's literal-vs-subject contract.
pub(super) fn check_literal_matches_subject(
    value: &Literal,
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !subject_ty.is_resolved() {
        return;
    }
    let lit_ty = literal_type(value, resolver.registry);
    if &lit_ty == subject_ty {
        return;
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
