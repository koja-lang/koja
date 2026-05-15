//! Control-flow expression resolution: `if` / `else`, `unless`,
//! `cond ... end`, the `?:` ternary, and `while`.
//!
//! `if`, `cond`, and ternary are value-producing when every reaching
//! arm tail type joins. The join is strict equality (no coercion)
//! with `Never` as the lattice bottom (`T ∪ Never = T`); divergent
//! arms (bodies that end in `return`) contribute `Never` and so
//! don't constrain the join.
//!
//! `unless` and `while` stay Unit-typed — loops are statement-shaped.
//!
//! `for` lives in [`crate::pipeline::synthesize::for_desugar`];
//! resolve never sees a statement-position `for`.
//!
//! [`body_tail_type`], [`join_arm_tails`], and
//! [`require_bool_condition`] are also consumed by
//! `super::match_expr`.

use expo_ast::ast::{CondArm, Diagnostic, Expr, Statement};
use expo_ast::identifier::ResolvedType;
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::moves::MoveLedgerSnapshot;
use super::types::{display_resolution, is_primitive, types_equivalent};
use super::walker::{resolve_body_with_expected, resolve_statement};
use crate::registry::GlobalRegistry;

pub(super) fn resolve_if(
    condition: &mut Expr,
    then_body: &mut [Statement],
    else_body: Option<&mut [Statement]>,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("if", condition, resolver.registry, diagnostics);
    // Snapshot the move ledger so each arm walks from the same
    // pre-branch state; merge pessimistically afterward so a move
    // in any arm shows up as (Maybe)Moved post-join. See
    // [`MoveLedger::merge_branches`].
    let baseline = resolver.moves.snapshot();
    resolve_body_with_expected(then_body, expected, resolver, diagnostics);
    let after_then = resolver.moves.snapshot();
    let Some(else_body) = else_body else {
        // No-`else` `if` is statement-shaped — there's no else-arm
        // to join, so the surface expression is `Unit`. Matches the
        // pre-block-params behavior; future "if-as-expression"
        // ergonomics that admit `if cond then 1 end` (Optional-typed
        // implicit None) is a separate slice. Treat the absent else
        // as a baseline-state arm so a move in `then` only becomes
        // `MaybeMoved` post-join, not `Moved`.
        resolver.moves.merge_branches(vec![after_then, baseline]);
        return resolver.registry.primitive("Unit");
    };
    resolver.moves.restore(baseline);
    resolve_body_with_expected(else_body, expected, resolver, diagnostics);
    let after_else = resolver.moves.snapshot();
    resolver.moves.merge_branches(vec![after_then, after_else]);
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
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("ternary", condition, resolver.registry, diagnostics);
    let baseline = resolver.moves.snapshot();
    resolve_expr_with_expected(then_expr, expected, resolver, diagnostics);
    let after_then = resolver.moves.snapshot();
    resolver.moves.restore(baseline);
    resolve_expr_with_expected(else_expr, expected, resolver, diagnostics);
    let after_else = resolver.moves.snapshot();
    resolver.moves.merge_branches(vec![after_then, after_else]);
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
    // `unless cond do BODY end` is `if !cond do BODY end` — there's
    // no else, so a body move only joins to MaybeMoved by merging
    // the body's post-state with the baseline.
    let baseline = resolver.moves.snapshot();
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let after_body = resolver.moves.snapshot();
    resolver.moves.merge_branches(vec![after_body, baseline]);
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
    arms: &mut [CondArm],
    else_body: Option<&mut [Statement]>,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let baseline = resolver.moves.snapshot();
    let mut branch_states: Vec<MoveLedgerSnapshot> = Vec::with_capacity(arms.len() + 1);
    let mut tails: Vec<(String, ResolvedType)> = Vec::with_capacity(arms.len() + 1);
    for (index, arm) in arms.iter_mut().enumerate() {
        resolver.moves.restore(baseline.clone());
        resolve_expr(&mut arm.condition, resolver, diagnostics);
        require_bool_condition("cond", &arm.condition, resolver.registry, diagnostics);
        resolve_body_with_expected(&mut arm.body, expected, resolver, diagnostics);
        branch_states.push(resolver.moves.snapshot());
        tails.push((
            format!("arm #{}", index + 1),
            body_tail_type(&arm.body, resolver.registry),
        ));
    }
    let else_tail = match else_body {
        Some(stmts) => {
            resolver.moves.restore(baseline.clone());
            resolve_body_with_expected(stmts, expected, resolver, diagnostics);
            branch_states.push(resolver.moves.snapshot());
            body_tail_type(stmts, resolver.registry)
        }
        None => {
            // Defensive: parser requires `else`, but if it ever
            // emits a `cond` without one, treat the missing arm as
            // the baseline so a move in any cond arm joins to
            // MaybeMoved rather than Moved.
            branch_states.push(baseline.clone());
            resolver.registry.primitive("Unit")
        }
    };
    resolver.moves.merge_branches(branch_states);
    tails.push(("else".to_string(), else_tail));
    join_arm_tails("cond", &tails, span, resolver.registry, diagnostics)
}

/// Resolve a `while cond ... end` loop. Condition must be `Bool`;
/// the body resolves under the same scope as anywhere else, with
/// `loop_depth` bumped and a fresh `loop_break_seen` slot pushed
/// so any inner `break` is gated to this loop and doesn't bleed
/// up to an outer enclosing loop. Result type is always `Unit`:
/// the cond-false fall-through means a `while` exits without
/// `break` even when the body contains one, so the divergent-
/// `Never` shape `resolve_loop` enables doesn't apply.
pub(super) fn resolve_while(
    condition: &mut Expr,
    body: &mut [Statement],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, resolver, diagnostics);
    require_bool_condition("while", condition, resolver.registry, diagnostics);
    resolver.loop_depth += 1;
    resolver.loop_break_seen.push(false);
    // One-pass conservative join: snapshot before the body, walk
    // once, merge with baseline so any move inside surfaces as
    // `MaybeMoved` post-loop (the body might run zero times). A
    // true fixpoint walk that catches "moved on iter 1, read on
    // iter 2" requires re-walking the body, which would re-mint
    // `LocalId`s through `LocalScope::declare`; deferred until the
    // scope is rotation-safe.
    let baseline = resolver.moves.snapshot();
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let after_body = resolver.moves.snapshot();
    resolver.moves.merge_branches(vec![baseline, after_body]);
    resolver.loop_break_seen.pop();
    resolver.loop_depth -= 1;
    resolver.registry.primitive("Unit")
}

/// Resolve a `loop ... end`. Bumps `loop_depth` (so `break` is
/// admitted) and pushes a fresh `loop_break_seen` slot. The result
/// type is `Never` when no `break` targeted this loop (the body
/// only exits via `return` / `panic` / not at all), and `Unit`
/// when at least one targeted break fired (the loop yields control
/// to its surroundings with no value at the break site).
pub(super) fn resolve_loop(
    body: &mut [Statement],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolver.loop_depth += 1;
    resolver.loop_break_seen.push(false);
    let baseline = resolver.moves.snapshot();
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let after_body = resolver.moves.snapshot();
    resolver.moves.merge_branches(vec![baseline, after_body]);
    let saw_break = resolver
        .loop_break_seen
        .pop()
        .expect("loop_break_seen push/pop balanced");
    resolver.loop_depth -= 1;
    if saw_break {
        resolver.registry.primitive("Unit")
    } else {
        resolver.registry.primitive("Never")
    }
}

/// Join exactly two arm tails (`if`/`else`'s shape). Separated from
/// the n-arm [`join_arm_tails`] so the diagnostic naming the
/// offending arms ("then" / "else") stays terse.
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
    if types_equivalent(then_ty, else_ty, registry) {
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
            Some((_, expected)) if types_equivalent(expected, ty, registry) => {}
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
