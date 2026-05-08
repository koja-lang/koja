//! `match` expression resolution. Walks the subject and every arm
//! body, requires a wildcard / binding catch-all, and joins the arm
//! tails using the same lattice [`super::control_flow`] uses for
//! `if` / `cond` / ternary.
//!
//! Guards (`when ...`) and unsupported pattern shapes diagnose
//! feature gaps so the surface stays well-defined.

use expo_ast::ast::{Diagnostic, Expr, MatchArm};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::control_flow::{body_tail_type, join_arm_tails};
use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::patterns::{is_match_subject_primitive, resolve_pattern};
use super::walker::resolve_statement;

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
    let mut tails: Vec<(String, ResolvedType)> = Vec::with_capacity(arms.len());
    for (index, arm) in arms.iter_mut().enumerate() {
        if let Some(guard) = &arm.guard {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support `when` guards in match arms",
                guard.span,
            ));
        }
        if matches!(arm.pattern, expo_ast::ast::Pattern::Literal { .. }) {
            has_literal_arm = true;
        }
        let arm_catch_all = resolve_pattern(&mut arm.pattern, &subject_ty, resolver, diagnostics);
        has_catch_all |= arm_catch_all;
        for stmt in &mut arm.body {
            resolve_statement(stmt, resolver, diagnostics);
        }
        tails.push((
            format!("arm #{}", index + 1),
            body_tail_type(&arm.body, resolver.registry),
        ));
    }

    if has_literal_arm
        && subject_ty.is_resolved()
        && !is_match_subject_primitive(&subject_ty, resolver.registry)
    {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet admit literal `match` patterns against \
             non-primitive subjects (supported subjects: `Bool` / `Int` / `Float` / `String`)",
            subject.span,
        ));
    }

    if !has_catch_all {
        diagnostics.push(Diagnostic::error(
            "match must include a wildcard `_` or binding catch-all arm",
            span,
        ));
    }

    join_arm_tails("match", &tails, span, resolver.registry, diagnostics)
}
