//! `match` expression resolution. Walks the subject and every arm
//! body, requires a wildcard / binding catch-all (or full structural
//! variant coverage for enum subjects), and joins the arm tails using
//! the same lattice [`super::control_flow`] uses for `if` / `cond` /
//! ternary.
//!
//! Arm guards (`pattern when expr -> body`) resolve in the
//! post-pattern-bind scope so the guard sees pattern-introduced
//! locals. Guarded arms are excluded from coverage attribution: a
//! guard can fail at runtime, so `Color.Red when ...` does not
//! cover `Red`.
//!
//! Reachability / redundancy is reported as warning-severity
//! diagnostics: arm-after-catch-all, duplicate enum variant or
//! literal across arms, and overlapping alternatives within a
//! single or-pattern. Warnings ride the `CheckedProgram`'s success
//! path; they do not gate IR lowering.
//!
//! `Bool` subjects relax the catch-all rule: if both `true` and
//! `false` literal arms appear (directly or as or-pattern
//! alternatives), the match is exhaustive without `_`.

use std::collections::BTreeSet;

use expo_ast::ast::{Diagnostic, Expr, MatchArm, Pattern};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::control_flow::{body_tail_type, join_arm_tails, require_bool_condition};
use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::patterns::{
    PatternCoverage, collect_literal_reprs, is_match_subject_primitive, match_subject_enum,
    resolve_pattern,
};
use super::types::{display_resolution, is_primitive};
use super::walker::resolve_statement;
use crate::registry::{EnumDefinition, GlobalRegistry};

pub(super) fn resolve_match(
    subject: &mut Expr,
    arms: &mut [MatchArm],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(subject, resolver, diagnostics);
    let subject_ty = subject.resolution.clone();

    if arms.is_empty() {
        diagnostics.push(Diagnostic::error("`match` requires at least one arm", span));
        return ResolvedType::unresolved();
    }

    let mut has_catch_all = false;
    let mut has_literal_arm = false;
    let mut covered_variants: Vec<u32> = Vec::new();
    let mut seen_literals: BTreeSet<String> = BTreeSet::new();
    let mut seen_variants: BTreeSet<u32> = BTreeSet::new();
    let mut tails: Vec<(String, ResolvedType)> = Vec::with_capacity(arms.len());
    for (index, arm) in arms.iter_mut().enumerate() {
        if matches!(arm.pattern, Pattern::Literal { .. }) {
            has_literal_arm = true;
        }
        let scope_snapshot = resolver.scope.snapshot();
        let coverage = resolve_pattern(&mut arm.pattern, &subject_ty, resolver, diagnostics);
        if let Some(guard) = &mut arm.guard {
            resolve_expr(guard, resolver, diagnostics);
            require_bool_condition("match arm guard", guard, resolver.registry, diagnostics);
        }
        for stmt in &mut arm.body {
            resolve_statement(stmt, resolver, diagnostics);
        }
        resolver.scope.restore(scope_snapshot);
        check_arm_reachability(
            arm,
            &coverage,
            has_catch_all,
            &seen_variants,
            &seen_literals,
            diagnostics,
        );
        if arm.guard.is_none() {
            match &coverage {
                PatternCoverage::CatchAll => has_catch_all = true,
                PatternCoverage::Variants(tags) => {
                    for tag in tags {
                        seen_variants.insert(*tag);
                    }
                    covered_variants.extend(tags);
                }
                PatternCoverage::Other => {
                    let mut literals: Vec<String> = Vec::new();
                    collect_literal_reprs(&arm.pattern, &mut literals);
                    for literal in literals {
                        seen_literals.insert(literal);
                    }
                }
            }
        }
        tails.push((
            format!("arm #{}", index + 1),
            body_tail_type(&arm.body, resolver.registry),
        ));
    }

    let subject_enum = match_subject_enum(&subject_ty, resolver.registry);
    if has_literal_arm
        && subject_ty.is_resolved()
        && subject_enum.is_none()
        && !is_match_subject_primitive(&subject_ty, resolver.registry)
    {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet admit literal `match` patterns against \
             non-primitive subjects (supported subjects: `Bool` / `Int` / `Float` / `String`)",
            subject.span,
        ));
    }

    if !has_catch_all {
        if let Some(definition) = subject_enum {
            diagnose_missing_enum_variants(definition, &covered_variants, span, diagnostics);
        } else if !is_bool_exhaustive(&subject_ty, &seen_literals, resolver.registry) {
            let subject_label = display_resolution(&subject_ty, resolver.registry);
            diagnostics.push(Diagnostic::error_with_hint(
                "match must include a wildcard `_` or binding catch-all arm",
                format!("the subject has type `{subject_label}`; add a catch-all `_ -> ...` arm"),
                span,
            ));
        }
    }

    join_arm_tails("match", &tails, span, resolver.registry, diagnostics)
}

/// Emit warning-severity reachability diagnostics for one arm.
/// Walks the catch-all-already-fired check first, then duplicate-
/// variant / duplicate-literal coverage against the rolling
/// accumulators. Does not mutate the accumulators — the caller
/// updates them after this returns so the warning is keyed on the
/// state the arm actually saw.
fn check_arm_reachability(
    arm: &MatchArm,
    coverage: &PatternCoverage,
    has_catch_all: bool,
    seen_variants: &BTreeSet<u32>,
    seen_literals: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if has_catch_all {
        diagnostics.push(Diagnostic::warning(
            "match arm is unreachable: a previous arm matches every value",
            arm.span,
        ));
        return;
    }
    match coverage {
        PatternCoverage::CatchAll => {}
        PatternCoverage::Variants(tags) => {
            if !tags.is_empty() && tags.iter().all(|tag| seen_variants.contains(tag)) {
                diagnostics.push(Diagnostic::warning(
                    "match arm is unreachable: every variant it covers is already \
                     matched by an earlier arm",
                    arm.span,
                ));
            }
        }
        PatternCoverage::Other => {
            let mut literals: Vec<String> = Vec::new();
            collect_literal_reprs(&arm.pattern, &mut literals);
            if !literals.is_empty() && literals.iter().all(|lit| seen_literals.contains(lit)) {
                diagnostics.push(Diagnostic::warning(
                    "match arm is unreachable: every literal it covers is already \
                     matched by an earlier arm",
                    arm.span,
                ));
            }
        }
    }
}

/// True when `subject_ty` is `Global.Bool` and both `true` and
/// `false` literal arms have already been collected. Used to short-
/// circuit the missing-catch-all error for fully-covered `Bool`
/// matches.
fn is_bool_exhaustive(
    subject_ty: &ResolvedType,
    seen_literals: &BTreeSet<String>,
    registry: &GlobalRegistry,
) -> bool {
    is_primitive(subject_ty, registry, "Bool")
        && seen_literals.contains("true")
        && seen_literals.contains("false")
}

fn diagnose_missing_enum_variants(
    definition: &EnumDefinition,
    covered: &[u32],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let missing: Vec<&str> = definition
        .variants
        .iter()
        .enumerate()
        .filter_map(|(index, variant)| {
            if covered.contains(&(index as u32)) {
                None
            } else {
                Some(variant.name.as_str())
            }
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    let plural = if missing.len() == 1 { "" } else { "s" };
    let missing_list = missing.join("`, `");
    diagnostics.push(Diagnostic::error_with_hint(
        format!("match against enum is not exhaustive: missing variant{plural} `{missing_list}`"),
        format!("add a catch-all `_ -> ...` arm or handle: `{missing_list}`"),
        span,
    ));
}
