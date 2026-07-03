//! Axis-type inference shared between list / map literals.
//!
//! A "literal axis" is a slot in the literal that needs an
//! inferred type: `[a, b, c]` has one axis (the element type),
//! `["k": v, ...]` has two (key + value). [`infer_axis`] walks the
//! axis's resolved entries and picks the floor, with a per-axis
//! diagnostic label and an example fragment for the empty-and-no-
//! hint case.
//!
//! The hint, when present, always wins: every entry whose
//! resolution disagrees emits a mismatch diagnostic but the hint
//! is what the caller stamps. Without a hint the floor is set by
//! the first resolved entry, and later entries that disagree diagnose
//! against the floor. An empty axis with no hint surfaces a
//! "cannot infer" diagnostic and returns `None`.

use koja_ast::ast::{Diagnostic, Expr};
use koja_ast::identifier::ResolvedType;
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::super::types::display_resolution;

/// Diagnostic phrasing for a single axis. Two strings rather than
/// one because the wording is two adjectives ("list literal
/// element" vs "map literal key") and inlining the join lets us
/// write each pair once at the call site without `format!`-ing.
pub(super) struct AxisLabel<'a> {
    /// Phrase identifying the literal kind, e.g. "list literal",
    /// "map literal".
    pub collection: &'a str,
    /// The axis name within that literal, e.g. "element", "key",
    /// "value".
    pub axis: &'a str,
}

/// Pick the resolved type for one axis. `hint` is the surrounding
/// expected-type contribution (the carrier's matching `type_args[i]`
/// slot, when fully resolved), and `entries` are the literal's
/// resolved values for this axis. Returns `None` (with a
/// diagnostic) when an empty axis has no hint to pin against.
///
/// `empty_example` is a minimal source fragment offered to the
/// user when the axis is empty and unhintable, e.g.
/// `result: List<Int> = []` for list, or
/// `result: Map<String, Int> = ["a": 1]` for map.
pub(super) fn infer_axis<'a, I>(
    entries: I,
    hint: Option<&ResolvedType>,
    label: AxisLabel<'_>,
    span: Span,
    empty_example: &str,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType>
where
    I: IntoIterator<Item = &'a Expr>,
{
    let entries: Vec<&Expr> = entries.into_iter().collect();
    if let Some(hint) = hint {
        for entry in &entries {
            if entry.resolution.is_resolved() && &entry.resolution != hint {
                diagnostics.push(mismatch(
                    label.collection,
                    label.axis,
                    hint,
                    entry,
                    resolver,
                ));
            }
        }
        return Some(hint.clone());
    }

    let mut chosen: Option<ResolvedType> = None;
    for entry in &entries {
        if !entry.resolution.is_resolved() {
            continue;
        }
        match &chosen {
            None => chosen = Some(entry.resolution.clone()),
            Some(prev) if prev != &entry.resolution => {
                diagnostics.push(mismatch(
                    label.collection,
                    label.axis,
                    prev,
                    entry,
                    resolver,
                ));
            }
            _ => {}
        }
    }

    if chosen.is_none() {
        diagnostics.push(Diagnostic::error(
            format!(
                "{} `[]` has no {} type. Annotate the binding or pass a context that \
                 determines the slot (e.g. `{}`)",
                label.collection, label.axis, empty_example,
            ),
            span,
        ));
    }
    chosen
}

/// Build a "{collection} {axis} type mismatch: expected `X`, found
/// `Y`" diagnostic at the offending entry's span.
fn mismatch(
    collection: &str,
    axis: &str,
    expected: &ResolvedType,
    actual: &Expr,
    resolver: &Resolver<'_>,
) -> Diagnostic {
    Diagnostic::error(
        format!(
            "{collection} {axis} type mismatch: expected `{}`, found `{}`",
            display_resolution(expected, resolver.registry),
            display_resolution(&actual.resolution, resolver.registry),
        ),
        actual.span,
    )
}
