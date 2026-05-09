//! Match-arm pattern resolution.
//!
//! Admits `Wildcard`, `Binding`, primitive `Literal`, `EnumUnit`,
//! `EnumTuple` / `EnumStruct` (one-level — elements / fields
//! restricted to wildcard / binding), `Struct` (same restriction),
//! `Or` (alternatives restricted to literal / EnumUnit, no
//! bindings), and `Constructor` shorthand (`Some(x)`, `None`,
//! `Ok(x)`, ...) — `Constructor` rewrites in place to the
//! corresponding `EnumTuple` / `EnumUnit` after looking the
//! variant up on the subject's enum, so seal / generics-substitute
//! / lowering never see the shape. Every other shape diagnoses a
//! feature gap. Returns [`PatternCoverage`] so
//! [`super::match_expr::resolve_match`] can run the
//! catch-all-or-exhaustiveness check without re-walking the arm.
//!
//! # Module layout
//!
//! - [`constructor`] — `Some(x)` / `None` / `Ok(x)` shorthand,
//!   rewritten in place to its qualified form.
//! - [`enums`] — `EnumUnit` / `EnumTuple` / `EnumStruct` shapes,
//!   plus the shared enum-lookup / generic-substitution helpers.
//! - [`structs`] — plain-struct destructure and the field-pattern
//!   walker shared with struct-shaped enum variants.
//! - [`or_pattern`] — `A | B | C` alternatives with intra-or-pattern
//!   reachability warnings.
//! - [`literals`] — literal-vs-subject type checking and the
//!   canonical literal-string representation used by the cross-arm
//!   reachability machinery.

mod constructor;
mod enums;
mod literals;
mod or_pattern;
mod structs;

use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::{Resolution, ResolvedType};
use expo_ast::labels::pattern_span;

use super::ctx::Resolver;
use super::types::is_primitive;
use crate::registry::{EnumDefinition, GlobalKind, GlobalRegistry};

use literals::literal_repr;

/// What a pattern admits at runtime. Drives the
/// catch-all-or-exhaustiveness rule in
/// [`super::match_expr::resolve_match`].
pub(super) enum PatternCoverage {
    /// Wildcard / binding — admits every value of the subject.
    CatchAll,
    /// `EnumUnit` / `EnumTuple` (or an `Or` of those) — admits
    /// exactly the listed variant tags.
    Variants(Vec<u32>),
    /// Literal patterns and `Or`s of literals. The arm fires for a
    /// specific runtime value but does not contribute to enum
    /// exhaustiveness; primitive subjects use the strict
    /// catch-all-required rule.
    Other,
}

pub(super) fn resolve_pattern(
    pat: &mut Pattern,
    subject_ty: &ResolvedType,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    match pat {
        Pattern::Binding { local_id, name, .. } => {
            let id = resolver.scope.declare(name, subject_ty.clone());
            *local_id = Some(id);
            PatternCoverage::CatchAll
        }
        Pattern::Constructor { .. } => {
            constructor::resolve_constructor_pattern(pat, subject_ty, resolver, diagnostics)
        }
        Pattern::EnumStruct {
            fields,
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_struct_pattern(
            type_path,
            variant,
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumTuple {
            elements,
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_tuple_pattern(
            type_path,
            variant,
            elements,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::EnumUnit {
            span,
            type_path,
            variant,
            ..
        } => enums::resolve_enum_unit_pattern(
            type_path,
            variant,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Literal { span, value } => {
            literals::check_literal_matches_subject(
                value,
                subject_ty,
                *span,
                resolver,
                diagnostics,
            );
            PatternCoverage::Other
        }
        Pattern::Or { patterns, span } => {
            or_pattern::resolve_or_pattern(patterns, subject_ty, *span, resolver, diagnostics)
        }
        Pattern::Struct {
            fields,
            span,
            type_path,
            ..
        } => structs::resolve_struct_pattern(
            type_path,
            fields,
            subject_ty,
            *span,
            resolver,
            diagnostics,
        ),
        Pattern::Wildcard { .. } => PatternCoverage::CatchAll,
        Pattern::Binary { .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support binary patterns (blocked on binary \
                 literals)",
                pattern_span(pat),
            ));
            PatternCoverage::Other
        }
        Pattern::List { .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support list patterns (blocked on alpha-IR \
                 list ops + a stable `List<T>` layout)",
                pattern_span(pat),
            ));
            PatternCoverage::Other
        }
        Pattern::TypedBinding { .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support typed-binding patterns (blocked on \
                 surface unions)",
                pattern_span(pat),
            ));
            PatternCoverage::Other
        }
    }
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

/// Lookup the [`EnumDefinition`] whose `Global(id)` head matches
/// `subject_ty`. Returns `None` for non-enum / unresolved subjects.
/// Used by [`super::match_expr::resolve_match`] to drive the
/// structural exhaustiveness check.
pub(super) fn match_subject_enum<'a>(
    subject_ty: &ResolvedType,
    registry: &'a GlobalRegistry,
) -> Option<&'a EnumDefinition> {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = subject_ty
    else {
        return None;
    };
    let entry = registry.get(*id)?;
    let GlobalKind::Enum(definition) = &entry.kind else {
        return None;
    };
    definition.as_ref()
}

/// Walk `pattern` and append a canonical string representation of
/// every `Literal` / `Or`-of-literal alternative it contains. Used
/// by [`super::match_expr::resolve_match`] to detect cross-arm
/// literal duplication (`1 -> _, 1 -> _`) without re-walking the
/// pattern's enum / struct / binding shapes.
pub(super) fn collect_literal_reprs(pattern: &Pattern, out: &mut Vec<String>) {
    match pattern {
        Pattern::Literal { value, .. } => out.push(literal_repr(value)),
        Pattern::Or { patterns, .. } => {
            for alt in patterns {
                collect_literal_reprs(alt, out);
            }
        }
        _ => {}
    }
}
