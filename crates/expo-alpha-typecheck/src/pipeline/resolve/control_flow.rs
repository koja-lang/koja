//! `if` / `unless` / `cond` resolution.
//!
//! `if` and `cond` are value-producing when every reaching arm tail
//! type joins. The join is strict equality (no coercion) with
//! `Never` as the lattice bottom (`T ∪ Never = T`); divergent arms
//! (bodies that end in `return`) contribute `Never` and so don't
//! constrain the join.
//!
//! `unless` stays Unit-typed — surface syntax doesn't admit a value
//! (no `else` arm), so there's nothing to join.
//!
//! `body_tail_type` is the per-body helper consulted by both
//! `resolve_if`'s join and `resolve_cond`'s. Mirrors v1's
//! `infer_body_type`: a body that contains any `Statement::Return`
//! is `Never`, otherwise the trailing `Statement::Expr`'s resolved
//! type, falling back to `Unit` for empty / non-`Expr` trailing
//! statements.

use expo_ast::ast::{Diagnostic, Expr, Statement};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use crate::registry::GlobalRegistry;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::{display_resolution, is_primitive};
use super::walker::resolve_statement;

pub(super) fn resolve_if(
    condition: &mut Expr,
    then_body: &mut [Statement],
    else_body: Option<&mut [Statement]>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("if", condition, resolver.registry, diagnostics);
    for stmt in then_body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let Some(else_body) = else_body else {
        // No-`else` `if` is statement-shaped — there's no else-arm
        // to join, so the surface expression is `Unit`. Matches the
        // pre-block-params behavior; future "if-as-expression"
        // ergonomics that admit `if cond then 1 end` (Optional-typed
        // implicit None) is a separate slice.
        return resolver.registry.primitive("Unit");
    };
    for stmt in else_body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let then_tail = body_tail_type(then_body, resolver.registry);
    let else_tail = body_tail_type(else_body, resolver.registry);
    join_two_arms(
        "if/else",
        ("then", &then_tail),
        ("else", &else_tail),
        span,
        resolver.registry,
        diagnostics,
    )
}

/// Resolve a `cond ? then_expr : else_expr` ternary. Same arm-tail
/// join semantics as `if`/`else` (strict equality with `Never` as
/// bottom), but the arms are expressions rather than statement
/// bodies so we read `expr.resolution` directly instead of routing
/// through `body_tail_type`. The parser disallows nested ternaries
/// — `a ? b ? c : d : e` is a parse error — so we only ever join
/// two arms here.
pub(super) fn resolve_ternary(
    condition: &mut Expr,
    then_expr: &mut Expr,
    else_expr: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("ternary", condition, resolver.registry, diagnostics);
    resolve_expr(then_expr, resolver, diagnostics);
    resolve_expr(else_expr, resolver, diagnostics);
    join_two_arms(
        "ternary",
        ("then", &then_expr.resolution),
        ("else", &else_expr.resolution),
        span,
        resolver.registry,
        diagnostics,
    )
}

pub(super) fn resolve_unless(
    condition: &mut Expr,
    body: &mut [Statement],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("unless", condition, resolver.registry, diagnostics);
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    resolver.registry.primitive("Unit")
}

/// Resolve a `cond ... end` chain: every arm's condition is a `Bool`,
/// every arm's body resolves, and the result type is the strict-
/// equality join of every arm tail type plus the else-body tail.
/// Mirrors v1's `cond` join (treating `Never` as bottom). Missing
/// `else_body` defaults to a Unit sink so a `cond` without an else
/// joins to `Unit` regardless of the arm tails — but the parser
/// requires `else`, so this branch is defensive.
pub(super) fn resolve_cond(
    arms: &mut [expo_ast::ast::CondArm],
    else_body: Option<&mut [Statement]>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let mut tails: Vec<(String, ResolvedType)> = Vec::with_capacity(arms.len() + 1);
    for (index, arm) in arms.iter_mut().enumerate() {
        resolve_expr(&mut arm.condition, resolver, diagnostics);
        require_bool_condition("cond", &arm.condition, resolver.registry, diagnostics);
        for stmt in arm.body.iter_mut() {
            resolve_statement(stmt, resolver, diagnostics);
        }
        tails.push((
            format!("arm #{}", index + 1),
            body_tail_type(&arm.body, resolver.registry),
        ));
    }
    let else_tail = match else_body {
        Some(stmts) => {
            for stmt in stmts.iter_mut() {
                resolve_statement(stmt, resolver, diagnostics);
            }
            body_tail_type(stmts, resolver.registry)
        }
        None => resolver.registry.primitive("Unit"),
    };
    tails.push(("else".to_string(), else_tail));
    join_arm_tails("cond", &tails, span, resolver.registry, diagnostics)
}

/// Join exactly two arm tails (`if`/`else`'s shape). Separated from
/// the n-arm `join_arm_tails` so the diagnostic naming the offending
/// arms ("then" / "else") stays terse.
fn join_two_arms(
    keyword: &str,
    then_tail: (&str, &ResolvedType),
    else_tail: (&str, &ResolvedType),
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let (then_label, then_ty) = then_tail;
    let (else_label, else_ty) = else_tail;
    if !then_ty.is_resolved() || !else_ty.is_resolved() {
        return ResolvedType::unresolved();
    }
    let then_never = is_never(then_ty, registry);
    let else_never = is_never(else_ty, registry);
    if then_never && else_never {
        return registry.primitive("Never");
    }
    if then_never {
        return else_ty.clone();
    }
    if else_never {
        return then_ty.clone();
    }
    if then_ty == else_ty {
        return then_ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "{keyword} arms have inconsistent types: {then_label}=`{}`, {else_label}=`{}`",
            display_resolution(then_ty, registry),
            display_resolution(else_ty, registry),
        ),
        span,
    ));
    ResolvedType::unresolved()
}

/// N-arm join used by `cond` and `match`. Returns the unique
/// non-`Never` tail type, `Never` when every tail diverges, or
/// `Unresolved` after a mismatch diagnostic. Unresolved tails
/// short-circuit the join (an upstream resolution error already
/// produced its own diagnostic).
pub(super) fn join_arm_tails(
    keyword: &str,
    tails: &[(String, ResolvedType)],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if tails.iter().any(|(_, t)| !t.is_resolved()) {
        return ResolvedType::unresolved();
    }
    let mut joined: Option<(&str, &ResolvedType)> = None;
    for (label, ty) in tails {
        if is_never(ty, registry) {
            continue;
        }
        match joined {
            None => joined = Some((label, ty)),
            Some((_, expected)) if expected == ty => {}
            Some((expected_label, expected)) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "{keyword} arms have inconsistent types: expected `{}` (from {expected_label}), \
                         got `{}` (from {label})",
                        display_resolution(expected, registry),
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                return ResolvedType::unresolved();
            }
        }
    }
    match joined {
        Some((_, ty)) => ty.clone(),
        None => registry.primitive("Never"),
    }
}

/// Compute a body's tail type. A body that contains any
/// `Statement::Return` is `Never` (the body diverges); otherwise
/// the trailing statement's type — `Statement::Expr` contributes
/// its resolved type, every other statement kind contributes `Unit`
/// (assignments / breaks have no value), and an empty body is
/// `Unit` too.
pub(super) fn body_tail_type(body: &[Statement], registry: &GlobalRegistry) -> ResolvedType {
    if body.iter().any(|s| matches!(s, Statement::Return { .. })) {
        return registry.primitive("Never");
    }
    match body.last() {
        Some(Statement::Expr(expr)) => expr.resolution.clone(),
        Some(_) | None => registry.primitive("Unit"),
    }
}

fn is_never(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    is_primitive(ty, registry, "Never")
}

/// Diagnose a non-Bool condition. Skips the check when the
/// condition itself failed to resolve — its own diagnostic is
/// already in flight.
pub(super) fn require_bool_condition(
    keyword: &str,
    condition: &Expr,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !condition.resolution.is_resolved() {
        return;
    }
    if !is_primitive(&condition.resolution, registry, "Bool") {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{keyword}` condition must be `Bool`, got `{}`",
                display_resolution(&condition.resolution, registry),
            ),
            condition.span,
        ));
    }
}
