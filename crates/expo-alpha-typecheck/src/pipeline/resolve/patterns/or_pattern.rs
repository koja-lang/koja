//! Or-pattern resolution: `A | B | C`. Alternatives are restricted
//! to literal / `EnumUnit` (no bindings) so coverage attribution
//! stays straightforward — every alternative reports its own
//! [`PatternCoverage`] and the whole or-pattern reports the
//! union.
//!
//! Intra-or-pattern reachability fires here as warnings: an
//! alternative whose tags / literals are entirely covered by an
//! earlier alternative in the same or-pattern is unreachable. The
//! cross-arm version (an alternative covered by an earlier *arm*'s
//! pattern) lives in [`super::super::match_expr`].

use std::collections::BTreeSet;

use expo_ast::ast::{Diagnostic, Pattern};
use expo_ast::identifier::ResolvedType;
use expo_ast::labels::{pattern_kind_label, pattern_span};
use expo_ast::span::Span;

use super::super::ctx::Resolver;
use super::literals::literal_repr;
use super::{PatternCoverage, resolve_pattern};

pub(super) fn resolve_or_pattern(
    patterns: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> PatternCoverage {
    if patterns.is_empty() {
        diagnostics.push(Diagnostic::error("or-pattern is empty", span));
        return PatternCoverage::Other;
    }
    let mut variant_tags: Vec<u32> = Vec::new();
    let mut all_literal = true;
    let mut all_enum_units = true;
    let mut seen_alt_literals: BTreeSet<String> = BTreeSet::new();
    let mut seen_alt_variants: BTreeSet<u32> = BTreeSet::new();
    for alternative in patterns.iter_mut() {
        if !is_admitted_or_alternative(alternative) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck only admits literal / enum-unit alternatives in \
                     or-patterns (got `{}`)",
                    pattern_kind_label(alternative),
                ),
                pattern_span(alternative),
            ));
            all_literal = false;
            all_enum_units = false;
            continue;
        }
        let alt_span = pattern_span(alternative);
        let pre_literal = match alternative {
            Pattern::Literal { value, .. } => Some(literal_repr(value)),
            _ => None,
        };
        match resolve_pattern(alternative, subject_ty, resolver, diagnostics) {
            PatternCoverage::Variants(tags) => {
                all_literal = false;
                let mut all_dup = !tags.is_empty();
                for tag in &tags {
                    if !seen_alt_variants.insert(*tag) {
                        continue;
                    }
                    all_dup = false;
                }
                if all_dup {
                    diagnostics.push(Diagnostic::warning(
                        "or-pattern alternative is unreachable: already listed earlier in \
                         this or-pattern",
                        alt_span,
                    ));
                }
                variant_tags.extend(tags);
            }
            PatternCoverage::Other => {
                all_enum_units = false;
                if let Some(repr) = pre_literal
                    && !seen_alt_literals.insert(repr)
                {
                    diagnostics.push(Diagnostic::warning(
                        "or-pattern alternative is unreachable: already listed earlier in \
                         this or-pattern",
                        alt_span,
                    ));
                }
            }
            PatternCoverage::CatchAll => {
                // Only reachable via an unhandled future shape; the
                // single-test guard above already rejects bindings /
                // wildcards inside or-patterns.
                all_literal = false;
                all_enum_units = false;
            }
        }
    }
    if all_enum_units && !all_literal {
        PatternCoverage::Variants(variant_tags)
    } else {
        PatternCoverage::Other
    }
}

fn is_admitted_or_alternative(pat: &Pattern) -> bool {
    matches!(pat, Pattern::EnumUnit { .. } | Pattern::Literal { .. })
}
