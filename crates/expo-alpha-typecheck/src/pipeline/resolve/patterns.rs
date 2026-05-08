//! Match-arm pattern resolution. Today admits `Wildcard`,
//! `Binding`, and primitive `Literal` patterns; every other shape
//! diagnoses a feature gap.
//!
//! Returns `is_catch_all` so [`super::match_expr::resolve_match`]
//! can validate the catch-all rule without re-walking the arm.

use expo_ast::ast::{Diagnostic, Literal, Pattern};
use expo_ast::identifier::ResolvedType;
use expo_ast::labels::{pattern_kind_label, pattern_span};

use crate::registry::GlobalRegistry;

use super::ctx::Resolver;
use super::ops::literal_type;
use super::types::{display_resolution, is_primitive};

pub(super) fn resolve_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match pat {
        Pattern::Wildcard { .. } => true,
        Pattern::Binding { local_id, name, .. } => {
            let id = resolver.scope.declare(name, subject_ty.clone());
            *local_id = Some(id);
            true
        }
        Pattern::Literal { value, span } => {
            check_literal_matches_subject(value, subject_ty, *span, resolver.registry, diagnostics);
            false
        }
        Pattern::Binary { .. }
        | Pattern::Constructor { .. }
        | Pattern::EnumStruct { .. }
        | Pattern::EnumTuple { .. }
        | Pattern::EnumUnit { .. }
        | Pattern::List { .. }
        | Pattern::Or { .. }
        | Pattern::Struct { .. }
        | Pattern::TypedBinding { .. } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support pattern `{}`",
                    pattern_kind_label(pat),
                ),
                pattern_span(pat),
            ));
            false
        }
    }
}

/// Diagnose when a `Pattern::Literal`'s value doesn't agree with
/// the subject type. No coercion — strict equality, matching the
/// rest of alpha's literal-vs-subject contract.
fn check_literal_matches_subject(
    value: &Literal,
    subject_ty: &ResolvedType,
    span: expo_ast::span::Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !subject_ty.is_resolved() {
        return;
    }
    let lit_ty = literal_type(value, registry);
    if &lit_ty == subject_ty {
        return;
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "match literal pattern of type `{}` does not match subject type `{}`",
            display_resolution(&lit_ty, registry),
            display_resolution(subject_ty, registry),
        ),
        span,
    ));
}

/// True when `subject_ty` resolves to a primitive admitted as a
/// literal-comparable subject (`Bool` / `Int` / `Float` / `String`).
/// Patterns made entirely of catch-alls bypass this check at the
/// `resolve_match` level — any subject type is fine when the only
/// patterns are wildcards / bindings.
pub(super) fn is_match_subject_primitive(
    subject_ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    is_primitive(subject_ty, registry, "Bool")
        || is_primitive(subject_ty, registry, "Float")
        || is_primitive(subject_ty, registry, "Int")
        || is_primitive(subject_ty, registry, "String")
}
