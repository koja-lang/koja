//! `spawn` / `receive` expression resolution.
//!
//! `spawn Type.start(config)` — validates that the inner expression
//! is a static `start` call on a struct/enum that implements
//! `Process<C, M, R>`. The spawn site's resolved type is
//! `Global.Ref<M, R>`, picked off the impl's recorded protocol args.
//!
//! `receive arms after timeout body end` — each arm's pattern
//! must be a [`Pattern::TypedBinding`] whose annotation resolves to
//! either a business envelope (`Pair<M, Option<ReplyTo<R>>>`) or
//! a lifecycle event (`Lifecycle`). Arms self-discriminate by their
//! envelope type, so no surface union plumbing is needed; v1's
//! `process_msg_type` envelope hint is unnecessary because
//! every arm carries the type explicitly. Arm bodies + the optional
//! `after` body join under the same lattice as `match` / `cond`.

use expo_ast::ast::{Diagnostic, Expr, ExprKind, MatchArm, Pattern, Statement};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::labels::pattern_span;
use expo_ast::span::Span;

use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};
use crate::registry::GlobalRegistry;

use super::control_flow::{body_tail_type, join_arm_tails, require_bool_condition};
use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::{display_resolution, is_primitive};
use super::walker::resolve_body_with_expected;

/// Resolve `spawn Type.start(config)`. The inner expression must be
/// a static method call to `start` on a struct/enum implementing
/// `Process<C, M, R>`. Returns `Global.Ref<M, R>`.
pub(super) fn resolve_spawn(
    inner: &mut Expr,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(inner, resolver, diagnostics);

    let receiver_id = match &inner.kind {
        ExprKind::MethodCall {
            receiver, method, ..
        } if method == "start" => match &receiver.resolution {
            ResolvedType::Named {
                resolution: Resolution::Global(id),
                ..
            } => Some(*id),
            _ => None,
        },
        _ => None,
    };

    let Some(target_id) = receiver_id else {
        diagnostics.push(Diagnostic::error(
            "`spawn` requires `Type.start(config)` where `Type` implements `Process`",
            span,
        ));
        return ResolvedType::unresolved();
    };

    let Some(process_id) = resolver
        .registry
        .lookup(&Identifier::new("Global", vec!["Process".to_string()]))
        .map(|(id, _)| id)
    else {
        diagnostics.push(Diagnostic::error(
            "`spawn` requires `Global.Process` in scope (autoimport `Global.process`)",
            span,
        ));
        return ResolvedType::unresolved();
    };

    let Some(protocol_args) = resolver
        .registry
        .lookup_conformance(target_id, process_id)
        .map(<[ResolvedType]>::to_vec)
    else {
        let target_label = resolver
            .registry
            .get(target_id)
            .map(|entry| entry.identifier.to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        diagnostics.push(Diagnostic::error(
            format!(
                "`{target_label}` does not implement `Process` — `spawn` requires a `Process` impl"
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };

    // Process<C, M, R>: pick M and R off the recorded args.
    let [_c, msg_ty, reply_ty] = protocol_args.as_slice() else {
        diagnostics.push(Diagnostic::error(
            "`Process` impl must have exactly three type arguments (C, M, R)",
            span,
        ));
        return ResolvedType::unresolved();
    };

    let Some(ref_id) = resolver
        .registry
        .lookup(&Identifier::new("Global", vec!["Ref".to_string()]))
        .map(|(id, _)| id)
    else {
        diagnostics.push(Diagnostic::error(
            "`spawn` requires `Global.Ref` in scope (autoimport `Global.process`)",
            span,
        ));
        return ResolvedType::unresolved();
    };

    ResolvedType::Named {
        resolution: Resolution::Global(ref_id),
        type_args: vec![msg_ty.clone(), reply_ty.clone()],
    }
}

/// Resolve `receive arms after timeout after_body end`. Each arm's
/// pattern must be a typed-binding whose annotation is either a
/// business envelope `Pair<M, Option<ReplyTo<R>>>` or a lifecycle
/// `Lifecycle`. Joins arm tails (and the after-body tail when an
/// `after` clause is present) under the same lattice `match` uses.
pub(super) fn resolve_receive(
    arms: &mut [MatchArm],
    after_timeout: Option<&mut Expr>,
    after_body: &mut [Statement],
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if arms.is_empty() {
        diagnostics.push(Diagnostic::error(
            "`receive` requires at least one arm",
            span,
        ));
        return ResolvedType::unresolved();
    }

    let mut tails: Vec<(String, ResolvedType)> = Vec::with_capacity(arms.len() + 1);
    for (index, arm) in arms.iter_mut().enumerate() {
        resolve_receive_arm(arm, expected, resolver, diagnostics);
        tails.push((
            format!("arm #{}", index + 1),
            body_tail_type(&arm.body, resolver.registry),
        ));
    }

    let after_present = after_timeout.is_some();
    if let Some(timeout) = after_timeout {
        resolve_expr(timeout, resolver, diagnostics);
        require_int_timeout(timeout, resolver.registry, diagnostics);
        resolve_body_with_expected(after_body, expected, resolver, diagnostics);
        tails.push((
            "after".to_string(),
            body_tail_type(after_body, resolver.registry),
        ));
    } else if !after_body.is_empty() {
        // Parser pairs `after` with the body; an empty timeout but a
        // populated body means the parser saw something unexpected.
        // Stay quiet — the parser already diagnosed.
        let _ = after_present;
    }

    join_arm_tails("receive", &tails, span, resolver.registry, diagnostics)
}

/// Resolve one receive arm. Validates the pattern is a typed-binding
/// against a business or lifecycle envelope, declares the bound name
/// into scope, then resolves the body under that scope. Stamps
/// `local_id` on the typed-binding so IR lower can reach the
/// binding without re-walking.
fn resolve_receive_arm(
    arm: &mut MatchArm,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let snapshot = resolver.scope.snapshot();
    let bound_type = bind_receive_pattern(&mut arm.pattern, resolver, diagnostics);
    if let Some(guard) = &mut arm.guard {
        resolve_expr(guard, resolver, diagnostics);
        require_bool_condition("receive arm guard", guard, resolver.registry, diagnostics);
    }
    resolve_body_with_expected(&mut arm.body, expected, resolver, diagnostics);
    resolver.scope.restore(snapshot);
    let _ = bound_type;
}

/// Validate the arm pattern is a typed-binding against one of the
/// admitted envelope types. Declares the bound name into scope and
/// stamps `local_id` on the pattern. Other pattern shapes diagnose.
fn bind_receive_pattern(
    pattern: &mut Pattern,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let Pattern::TypedBinding {
        local_id,
        name,
        resolved_type,
        type_expr,
        span,
    } = pattern
    else {
        diagnostics.push(Diagnostic::error(
            "receive arms must use a typed-binding pattern \
             (`name: Pair<M, Option<ReplyTo<R>>>` or `name: Lifecycle`)",
            pattern_span(pattern),
        ));
        return None;
    };
    let resolved = resolve_type_expr(
        type_expr,
        TypeParamScope::new(resolver.type_param_owners),
        resolver.resolution_scope(),
        diagnostics,
    );
    if !resolved.is_resolved() {
        return None;
    }
    if !is_business_envelope(&resolved, resolver.registry)
        && !is_lifecycle(&resolved, resolver.registry)
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "receive only supports business (`Pair<M, Option<ReplyTo<R>>>`) and \
                 lifecycle (`Lifecycle`) arms (got `{}`)",
                display_resolution(&resolved, resolver.registry),
            ),
            *span,
        ));
        return None;
    }
    let id = resolver.scope.declare(name, resolved.clone());
    *local_id = Some(id);
    *resolved_type = Some(resolved.clone());
    Some(resolved)
}

/// `Pair<_, Option<ReplyTo<_>>>`. Walks the head id and inner shape
/// without caring what `M` / `R` resolve to — every concrete `M` /
/// `R` is admissible at the receive site.
fn is_business_envelope(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(head_id),
        type_args,
    } = ty
    else {
        return false;
    };
    if !is_global_type(*head_id, registry, "Pair") {
        return false;
    }
    let [_msg, second] = type_args.as_slice() else {
        return false;
    };
    let ResolvedType::Named {
        resolution: Resolution::Global(option_id),
        type_args: option_args,
    } = second
    else {
        return false;
    };
    if !is_global_type(*option_id, registry, "Option") {
        return false;
    }
    let [option_inner] = option_args.as_slice() else {
        return false;
    };
    let ResolvedType::Named {
        resolution: Resolution::Global(reply_id),
        type_args: reply_args,
    } = option_inner
    else {
        return false;
    };
    is_global_type(*reply_id, registry, "ReplyTo") && reply_args.len() == 1
}

fn is_lifecycle(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    is_primitive_named(ty, registry, "Lifecycle")
}

/// Like `is_primitive`, but admits both stub-shaped struct primitives
/// and user-package `Global.<name>` enums (`Lifecycle` lives in
/// `Global.process`, not the preloaded stub set).
fn is_primitive_named(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = ty
    else {
        return false;
    };
    if !type_args.is_empty() {
        return false;
    }
    is_global_type(*id, registry, name)
}

fn is_global_type(id: GlobalRegistryId, registry: &GlobalRegistry, name: &str) -> bool {
    let Some(entry) = registry.get(id) else {
        return false;
    };
    entry.identifier.is_in_global() && entry.identifier.last() == name
}

fn require_int_timeout(
    timeout: &Expr,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !timeout.resolution.is_resolved() {
        return;
    }
    if !is_primitive(&timeout.resolution, registry, "Int") {
        diagnostics.push(Diagnostic::error(
            format!(
                "`receive after` timeout must be `Int`, got `{}`",
                display_resolution(&timeout.resolution, registry),
            ),
            timeout.span,
        ));
    }
}
