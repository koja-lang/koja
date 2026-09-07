//! `match` expression resolution. Walks the subject and every arm
//! body, checks coverage, and joins the arm tails using the same
//! lattice [`super::control_flow`] uses for `if` / `cond` / ternary.
//! With no expected type the arms hint each other (see
//! [`super::speculation::resolve_arms`]).
//!
//! Coverage is a usefulness question (see
//! [`super::patterns::SubjectCoverage`]). The match is exhaustive
//! when a wildcard row is not useful against the unguarded arms,
//! and nested payload patterns combine, so `Some(Red)`, `Some(Green)`
//! and `None` exhaust an `Option<Color>` with two colors. A missing
//! case prints as a witness pattern the user can paste into a new
//! arm.
//!
//! Arm guards (`pattern when expr -> body`) resolve in the
//! post-pattern-bind scope so the guard sees pattern-introduced
//! locals. A guard can fail at runtime, so guarded arms never join
//! the coverage matrix, but they are still tested for reachability.
//!
//! Reachability is reported as warning-severity diagnostics. An arm
//! (or one alternative of an or-pattern) whose row is not useful
//! against the rows above it is unreachable. Warnings ride the
//! `CheckedProgram`'s success path, they do not gate IR lowering.

use koja_ast::ast::{Diagnostic, Expr, MatchArm, Pattern};
use koja_ast::identifier::ResolvedType;
use koja_ast::labels::pattern_span;
use koja_ast::span::Span;

use super::control_flow::{body_tail_type, join_arm_tails, require_bool_condition};
use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::patterns::{
    DeconstructedPattern, SubjectCoverage, is_enum_or_union_subject, is_match_subject_primitive,
    resolve_pattern,
};
use super::speculation::{ArmSet, ArmTail, Restorable, resolve_arms};
use super::types::display_resolution;
use super::walker::resolve_body_with_expected;
use crate::registry::GlobalRegistry;

/// Missing patterns listed before the diagnostic collapses the rest
/// into a count.
const MAX_LISTED_WITNESSES: usize = 3;

pub(super) fn resolve_match(
    subject: &mut Expr,
    arms: &mut Vec<MatchArm>,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(subject, resolver, diagnostics);
    resolve_match_arms(
        "match",
        subject,
        arms,
        expected,
        span,
        resolver,
        diagnostics,
    )
}

/// The arm half of [`resolve_match`], split out so the `try` /
/// `rescue` desugars in [`super::error_channel`] can resolve their
/// synthesized arms against an already-resolved subject (which must
/// not be walked twice). `keyword` labels the join diagnostics with
/// the construct the user actually wrote.
pub(super) fn resolve_match_arms(
    keyword: &str,
    subject: &Expr,
    arms: &mut Vec<MatchArm>,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let subject_ty = subject.resolution.clone();

    if arms.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!("`{keyword}` requires at least one arm"),
            span,
        ));
        return ResolvedType::unresolved();
    }

    let has_literal_arm = arms
        .iter()
        .any(|arm| matches!(arm.pattern, Pattern::Literal { .. }));
    let tails = resolve_arms(
        &mut MatchArms {
            arms,
            keyword,
            subject_ty: &subject_ty,
        },
        expected,
        resolver,
        diagnostics,
    );

    if has_literal_arm
        && subject_ty.is_resolved()
        && !is_enum_or_union_subject(&subject_ty, resolver.registry)
        && !is_match_subject_primitive(&subject_ty, resolver.registry)
    {
        diagnostics.push(Diagnostic::error_with_hint(
            "typecheck does not yet admit literal `match` patterns against \
             non-primitive subjects",
            "literal patterns are supported for `Bool`, `String`, and numeric subjects",
            subject.span,
        ));
    }

    check_coverage(arms, &subject_ty, span, resolver.registry, diagnostics);

    join_arm_tails(keyword, &tails, span, resolver.registry, diagnostics)
}

/// The arms of one `match`, resolved against the subject's type.
struct MatchArms<'a> {
    arms: &'a mut Vec<MatchArm>,
    keyword: &'a str,
    subject_ty: &'a ResolvedType,
}

impl Restorable for MatchArms<'_> {
    type Saved = Vec<MatchArm>;

    fn save(&self) -> Vec<MatchArm> {
        self.arms.clone()
    }

    fn restore(&mut self, saved: Vec<MatchArm>) {
        *self.arms = saved;
    }
}

impl ArmSet for MatchArms<'_> {
    /// Resolve every arm's pattern, guard, and body against `hint`.
    fn resolve(
        &mut self,
        hint: Option<&ResolvedType>,
        resolver: &mut Resolver<'_>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Vec<ArmTail> {
        let mut tails = Vec::with_capacity(self.arms.len());
        for (index, arm) in self.arms.iter_mut().enumerate() {
            let scope_snapshot = resolver.scope.snapshot();
            resolve_pattern(&mut arm.pattern, self.subject_ty, resolver, diagnostics);
            if let Some(guard) = &mut arm.guard {
                resolve_expr(guard, resolver, diagnostics);
                require_bool_condition("match arm guard", guard, resolver.registry, diagnostics);
            }
            resolve_body_with_expected(&mut arm.body, hint, resolver, diagnostics);
            resolver.scope.restore(scope_snapshot);
            tails.push((
                arm_label(self.keyword, index),
                body_tail_type(&arm.body, resolver.registry),
            ));
        }
        tails
    }
}

/// Join-diagnostic label for one arm. A desugared `rescue` names
/// its two synthesized arms by role so the message reads in the
/// user's terms rather than exposing the underlying `match`.
fn arm_label(keyword: &str, index: usize) -> String {
    match (keyword, index) {
        ("rescue", 0) => "the subject's `Ok` value".to_string(),
        ("rescue", _) => "the rescue handler".to_string(),
        _ => format!("arm #{}", index + 1),
    }
}

/// Reachability warnings per arm, then the exhaustiveness error.
/// Skipped when any arm fails to deconstruct, since resolution
/// already reported the shape mismatch.
fn check_coverage(
    arms: &[MatchArm],
    subject_ty: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let coverage = SubjectCoverage::new(subject_ty, registry);
    let Some(rows) = arms
        .iter()
        .map(|arm| coverage.deconstruct(&arm.pattern))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };

    let mut matrix: Vec<&DeconstructedPattern> = Vec::with_capacity(arms.len());
    for (arm, row) in arms.iter().zip(&rows) {
        check_arm_reachability(arm, row, &matrix, &coverage, diagnostics);
        if arm.guard.is_none() {
            matrix.push(row);
        }
    }

    let missing = coverage.missing_patterns(&matrix);
    if missing.is_empty() {
        return;
    }
    if missing.iter().any(|witness| witness == "_") {
        let subject_label = display_resolution(subject_ty, registry);
        diagnostics.push(Diagnostic::error_with_hint(
            "match must include a wildcard `_` or binding catch-all arm",
            format!("the subject has type `{subject_label}`, so add a catch-all `_ -> ...` arm"),
            span,
        ));
        return;
    }
    let listed = format_witness_list(&missing);
    diagnostics.push(Diagnostic::error_with_hint(
        format!("match is not exhaustive. Missing pattern(s) {listed}"),
        format!("add a catch-all `_ -> ...` arm or handle {listed}"),
        span,
    ));
}

/// Warn when no value can reach `row` past the unguarded rows above
/// it. Or-pattern alternatives are tested one at a time, against the
/// earlier arms plus the alternatives before them, so a duplicate
/// inside one or-pattern warns too.
fn check_arm_reachability<'a>(
    arm: &MatchArm,
    row: &'a DeconstructedPattern,
    earlier: &[&'a DeconstructedPattern],
    coverage: &SubjectCoverage<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if earlier
        .iter()
        .any(|other| **other == DeconstructedPattern::Wildcard)
    {
        diagnostics.push(Diagnostic::warning(
            "match arm is unreachable because a previous arm matches every value",
            arm.span,
        ));
        return;
    }
    let (DeconstructedPattern::Or(alternatives), Pattern::Or { patterns, .. }) =
        (row, &arm.pattern)
    else {
        if !coverage.is_useful(earlier, row) {
            diagnostics.push(Diagnostic::warning(
                "match arm is unreachable because earlier arms already match every value it \
                 covers",
                arm.span,
            ));
        }
        return;
    };
    let mut seen: Vec<&DeconstructedPattern> = earlier.to_vec();
    for (alternative, pattern) in alternatives.iter().zip(patterns) {
        if !coverage.is_useful(&seen, alternative) {
            diagnostics.push(Diagnostic::warning(
                "or-pattern alternative is unreachable because earlier arms or alternatives \
                 already match every value it covers",
                pattern_span(pattern),
            ));
        }
        seen.push(alternative);
    }
}

fn format_witness_list(missing: &[String]) -> String {
    let listed: Vec<String> = missing
        .iter()
        .take(MAX_LISTED_WITNESSES)
        .map(|witness| format!("`{witness}`"))
        .collect();
    let hidden = missing.len().saturating_sub(MAX_LISTED_WITNESSES);
    if hidden == 0 {
        listed.join(", ")
    } else {
        format!("{} and {hidden} more", listed.join(", "))
    }
}
