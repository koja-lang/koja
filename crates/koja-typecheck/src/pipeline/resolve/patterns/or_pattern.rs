//! Or-pattern resolution: `A | B | C`. Alternatives are restricted
//! to literal / `EnumUnit` (no bindings) so the arm binds nothing
//! whichever alternative fires. Reachability of each alternative,
//! within the or-pattern and against earlier arms, is a usefulness
//! question answered in [`super::super::match_expr`]. An `EnumUnit`
//! alternative on a union subject is an error, because it would match
//! through the union.

use koja_ast::ast::{Diagnostic, Pattern};
use koja_ast::identifier::ResolvedType;
use koja_ast::labels::{pattern_kind_label, pattern_span};
use koja_ast::span::Span;

use super::super::ctx::Resolver;
use super::resolve_pattern;

pub(super) fn resolve_or_pattern(
    patterns: &mut [Pattern],
    subject_ty: &ResolvedType,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if patterns.is_empty() {
        diagnostics.push(Diagnostic::error("or-pattern is empty", span));
        return;
    }
    for alternative in patterns.iter_mut() {
        if !is_admitted_or_alternative(alternative) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typecheck only admits literal / enum-unit alternatives in \
                     or-patterns (got `{}`)",
                    pattern_kind_label(alternative),
                ),
                pattern_span(alternative),
            ));
            continue;
        }
        resolve_pattern(alternative, subject_ty, resolver, diagnostics);
        // On a union subject, a variant alternative is two tests in
        // sequence, the member and then the variant. The or-chain
        // lowering has one test for each alternative.
        if matches!(alternative, Pattern::UnionMember { .. }) {
            diagnostics.push(Diagnostic::error_with_hint(
                "an or-pattern cannot hold a variant pattern when the subject is a union",
                "write one arm for each alternative",
                pattern_span(alternative),
            ));
        }
    }
}

fn is_admitted_or_alternative(pat: &Pattern) -> bool {
    matches!(pat, Pattern::EnumUnit { .. } | Pattern::Literal { .. })
}
