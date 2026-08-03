//! The `! E` error channel: `try` / `fail` / `rescue` desugaring
//! plus `Result.Ok` auto-wrapping for `!`-spelled functions.
//!
//! `-> T ! E` already lifted to `Result<T, E>`, so everything here
//! is resolve-time lowering onto existing shapes: `try expr` and
//! `expr rescue e -> handler` become `match` expressions over the
//! resolved subject, and `fail expr` becomes
//! `return Result.Err(expr)` with the payload widened into the
//! declared error type. Every synthesized node resolves exactly
//! once. In particular a desugared `return` is never re-walked,
//! since re-checking it against the Ok-wrapped expected type would
//! misfire.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, Literal, MatchArm, Pattern, Statement,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::coercion::check_compatible_stamping;
use super::ctx::Resolver;
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::match_expr::resolve_match_arms;
use super::types::{display_resolution, is_primitive, peel_alias, types_equivalent};
use crate::registry::{FunctionSignature, GlobalRegistry};

/// Binder names for the synthesized `match` arms. Leading
/// underscores keep them out of the way of user locals. Pattern
/// bindings shadow, so a collision is harmless anyway.
const RESCUE_OK_BINDER: &str = "__rescue_ok";
const TRY_ERR_BINDER: &str = "__try_err";
const TRY_OK_BINDER: &str = "__try_ok";

/// The enclosing function-shape's error channel, present when the
/// declared return type is `Result<T, E>` under either spelling.
/// `ok_wraps` is true only for the `-> T ! E` spelling, where
/// success values wrap in `Result.Ok` automatically.
#[derive(Clone)]
pub(super) struct ErrorChannel {
    /// The declared error type `E`.
    pub error: ResolvedType,
    /// The declared success type `T`.
    pub ok: ResolvedType,
    /// True when the function was declared with `! E`, so `return`
    /// values and the trailing expression check as `T` and wrap.
    pub ok_wraps: bool,
    /// The full `Result<T, E>` as declared, preserved verbatim for
    /// stamping synthesized constructions.
    pub result: ResolvedType,
}

/// Project a lifted signature onto its error channel. `None` when
/// the declared return type is not a `Result`.
pub(super) fn channel_for_signature(
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
) -> Option<ErrorChannel> {
    channel_for_return(
        &signature.return_type,
        signature.declared_fallible,
        registry,
    )
}

/// Error channel for a closure boundary. Closures have no `!`
/// spelling, so `try` and `fail` resolve against a `Result` return
/// type but success values never auto-wrap.
pub(super) fn channel_for_closure(
    return_type: Option<&ResolvedType>,
    registry: &GlobalRegistry,
) -> Option<ErrorChannel> {
    channel_for_return(return_type?, false, registry)
}

fn channel_for_return(
    return_type: &ResolvedType,
    ok_wraps: bool,
    registry: &GlobalRegistry,
) -> Option<ErrorChannel> {
    let (ok, error) = result_type_args(return_type, registry)?;
    Some(ErrorChannel {
        error,
        ok,
        ok_wraps,
        result: return_type.clone(),
    })
}

/// Resolve `try expr`, rewriting the node in place to
///
/// ```text
/// match expr
///   Ok(__try_ok) -> __try_ok
///   Err(__try_err) -> fail __try_err
/// end
/// ```
///
/// The synthesized `fail` arm reuses the statement-position `fail`
/// desugar, so error widening and its diagnostics live in one
/// place. The whole expression types as the subject's Ok type.
pub(super) fn resolve_try(
    expr: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let span = expr.span;
    let taken = std::mem::replace(
        &mut expr.kind,
        ExprKind::Literal {
            value: Literal::Unit,
        },
    );
    let ExprKind::Try { expr: mut subject } = taken else {
        unreachable!("resolve_try dispatched on a non-Try expression");
    };
    resolve_expr(&mut subject, resolver, diagnostics);
    let subject_result = result_type_args(&subject.resolution, resolver.registry);
    if subject.resolution.is_resolved() {
        if resolver.error_channel.is_none() {
            diagnostics.push(Diagnostic::error_with_hint(
                "`try` needs an error channel on the enclosing function",
                "declare the return type as `-> T ! E` so the propagated error \
                 has somewhere to go",
                span,
            ));
        } else if subject_result.is_none() {
            diagnostics.push(non_result_subject_diagnostic(
                "try",
                &subject,
                resolver.registry,
            ));
        }
    }
    if resolver.error_channel.is_none() || subject_result.is_none() {
        expr.kind = ExprKind::Try { expr: subject };
        return ResolvedType::unresolved();
    }
    let fail_tail = Expr::new(
        ExprKind::Fail {
            value: Box::new(ident_expr(TRY_ERR_BINDER, span)),
        },
        span,
    );
    let mut arms = vec![
        unwrap_arm(TRY_OK_BINDER, span),
        variant_arm(
            "Err",
            binding_pattern(TRY_ERR_BINDER, span),
            fail_tail,
            span,
        ),
    ];
    let ty = resolve_match_arms(
        "try",
        &subject,
        &mut arms,
        None,
        span,
        resolver,
        diagnostics,
    );
    expr.kind = ExprKind::Match { subject, arms };
    ty
}

/// Resolve `subject rescue binder -> handler`, rewriting the node
/// in place to
///
/// ```text
/// match subject
///   Ok(__rescue_ok) -> __rescue_ok
///   Err(binder) -> handler
/// end
/// ```
///
/// The handler resolves with the subject's Ok type expected, so it
/// must produce that type or diverge (`fail`, `return`, a panic).
pub(super) fn resolve_rescue(
    expr: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let span = expr.span;
    let taken = std::mem::replace(
        &mut expr.kind,
        ExprKind::Literal {
            value: Literal::Unit,
        },
    );
    let ExprKind::Rescue {
        mut subject,
        binder,
        binder_span,
        handler,
    } = taken
    else {
        unreachable!("resolve_rescue dispatched on a non-Rescue expression");
    };
    resolve_expr(&mut subject, resolver, diagnostics);
    let Some((ok_ty, _)) = result_type_args(&subject.resolution, resolver.registry) else {
        if subject.resolution.is_resolved() {
            diagnostics.push(non_result_subject_diagnostic(
                "rescue",
                &subject,
                resolver.registry,
            ));
        }
        expr.kind = ExprKind::Rescue {
            subject,
            binder,
            binder_span,
            handler,
        };
        return ResolvedType::unresolved();
    };
    let error_pattern = match &binder {
        Some(name) => binding_pattern(name, binder_span),
        None => Pattern::Wildcard { span: binder_span },
    };
    let mut arms = vec![
        unwrap_arm(RESCUE_OK_BINDER, span),
        variant_arm("Err", error_pattern, *handler, span),
    ];
    let ty = resolve_match_arms(
        "rescue",
        &subject,
        &mut arms,
        Some(&ok_ty),
        span,
        resolver,
        diagnostics,
    );
    expr.kind = ExprKind::Match { subject, arms };
    ty
}

/// True when `stmt` is a statement-position `fail`, the shape
/// [`resolve_fail_statement`] rewrites. Expression-position `fail`
/// falls through to the dispatch rejection in [`super::expr`].
pub(super) fn is_fail_statement(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Expr(Expr {
            kind: ExprKind::Fail { .. },
            ..
        })
    )
}

/// Resolve a statement-position `fail expr`, rewriting the whole
/// statement to `return Result.Err(expr)` with the payload widened
/// into the enclosing error type. The synthesized `return` is
/// fully resolved here and never re-walked, so it bypasses the
/// Ok-wrapping `return` path by construction.
pub(super) fn resolve_fail_statement(
    stmt: &mut Statement,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Statement::Expr(expr) = stmt else {
        unreachable!("resolve_fail_statement dispatched on a non-fail statement");
    };
    let span = expr.span;
    let ExprKind::Fail { value } = &mut expr.kind else {
        unreachable!("resolve_fail_statement dispatched on a non-fail statement");
    };
    let Some(channel) = resolver.error_channel.clone() else {
        resolve_expr(value, resolver, diagnostics);
        diagnostics.push(Diagnostic::error_with_hint(
            "`fail` needs an error channel on the enclosing function",
            "declare the return type as `-> T ! E` so the error has somewhere to go",
            span,
        ));
        return;
    };
    resolve_expr_with_expected(value, Some(&channel.error), resolver, diagnostics);
    let actual = value.resolution.clone();
    if actual.is_resolved()
        && !is_primitive(&actual, resolver.registry, "Never")
        && check_compatible_stamping(value, &actual, &channel.error, resolver.registry).is_some()
    {
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "`fail` sends `{}`, but the enclosing function declares error type `{}`",
                display_resolution(&actual, resolver.registry),
                display_resolution(&channel.error, resolver.registry),
            ),
            "widen the declared error union to include this type, or rescue it \
             into a declared one",
            value.span,
        ));
    }
    let payload = std::mem::replace(
        value.as_mut(),
        Expr::new(
            ExprKind::Literal {
                value: Literal::Unit,
            },
            span,
        ),
    );
    *stmt = Statement::Return {
        value: Some(result_construction("Err", payload, &channel.result, span)),
        span,
    };
}

/// Detect a hand-written `Result.Ok(...)` / `Result.Err(...)` in a
/// return position of a `!`-spelled function whose success type is
/// not itself a `Result`. Resolving such a value against the
/// unwrapped success type dead-ends in `E`-inference noise, so the
/// caller retargets resolution at the full `Result` type and lets
/// the mismatch check surface the auto-wrap rule instead.
pub(super) fn hand_wrapped_result(
    expr: &Expr,
    channel: &ErrorChannel,
    registry: &GlobalRegistry,
) -> bool {
    if !channel.ok_wraps || result_type_args(&channel.ok, registry).is_some() {
        return false;
    }
    matches!(
        &expr.kind,
        ExprKind::EnumConstruction { type_path, .. }
            if type_path.len() == 1 && type_path[0] == "Result"
    )
}

/// The expected type for a return-position expression in a
/// `!`-spelled function: the unwrapped success type, except a
/// [`hand_wrapped_result`] retargets at the full `Result` so its
/// teacher diagnostic can fire.
pub(super) fn return_site_expected(
    expr: &Expr,
    default: Option<&ResolvedType>,
    resolver: &Resolver<'_>,
) -> Option<ResolvedType> {
    if let Some(channel) = &resolver.error_channel
        && hand_wrapped_result(expr, channel, resolver.registry)
    {
        return Some(channel.result.clone());
    }
    default.cloned()
}

/// After a `return` site in a `!`-spelled function checks its value
/// against the unwrapped success type, wrap it in `Result.Ok`. A
/// bare `return` becomes `return Result.Ok(())`.
pub(super) fn ok_wrap_return(value: &mut Option<Expr>, span: Span, resolver: &Resolver<'_>) {
    let Some(channel) = &resolver.error_channel else {
        return;
    };
    if !channel.ok_wraps {
        return;
    }
    match value {
        // A value already typed as the full `Result` is the
        // [`hand_wrapped_result`] error path. Its diagnostic is in
        // flight, so don't stack a second wrapper on top.
        Some(inner) if types_equivalent(&inner.resolution, &channel.result, resolver.registry) => {}
        Some(inner) => ok_wrap_expr(inner, &channel.result),
        None => {
            *value = Some(ok_unit_construction(
                &channel.result,
                span,
                resolver.registry,
            ));
        }
    }
}

/// Replace `*expr` in place with `Result.Ok(<original>)`, stamped
/// with the full declared `Result` type. Mirrors the in-place
/// rewrite shape of [`super::strings::resolve_string`]'s
/// `.format()` wrapping.
pub(super) fn ok_wrap_expr(expr: &mut Expr, result: &ResolvedType) {
    let span = expr.span;
    let original = std::mem::replace(
        expr,
        Expr::new(
            ExprKind::Literal {
                value: Literal::Unit,
            },
            span,
        ),
    );
    *expr = result_construction("Ok", original, result, span);
}

/// A fully stamped `Result.Ok(())` for the fall-off-the-end return
/// value of a `-> () ! E` function.
pub(super) fn ok_unit_construction(
    result: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
) -> Expr {
    let mut unit = Expr::new(
        ExprKind::Literal {
            value: Literal::Unit,
        },
        span,
    );
    unit.resolution = registry.primitive("Unit");
    result_construction("Ok", unit, result, span)
}

/// Build a `Result.<variant>(payload)` construction stamped with
/// the declared `Result` type. IR lowering reads the variant name
/// and the stamped resolution, so no further resolve pass is
/// needed (or safe) on the synthesized node.
fn result_construction(variant: &str, payload: Expr, result: &ResolvedType, span: Span) -> Expr {
    let mut construction = Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec!["Result".to_string()],
            variant: variant.to_string(),
            data: EnumConstructionData::Tuple(vec![payload]),
        },
        span,
    );
    construction.resolution = result.clone();
    construction
}

/// Split `ty` into `(ok, error)` when it peels to
/// `Global.Result<T, E>`.
fn result_type_args(
    ty: &ResolvedType,
    registry: &GlobalRegistry,
) -> Option<(ResolvedType, ResolvedType)> {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = peel_alias(ty, registry)
    else {
        return None;
    };
    if !is_global_named(id, "Result", registry) || type_args.len() != 2 {
        return None;
    }
    let mut args = type_args.into_iter();
    Some((args.next().unwrap(), args.next().unwrap()))
}

fn is_global_named(id: GlobalRegistryId, name: &str, registry: &GlobalRegistry) -> bool {
    let target = Identifier::new("Global", vec![name.to_string()]);
    registry
        .lookup(&target)
        .is_some_and(|(target_id, _)| target_id == id)
}

/// The non-`Result` subject rejection for `try` / `rescue`, with an
/// `Option`-specific hint pointing at `or_err`.
fn non_result_subject_diagnostic(
    keyword: &str,
    subject: &Expr,
    registry: &GlobalRegistry,
) -> Diagnostic {
    let message = format!(
        "`{keyword}` needs a `Result` subject, got `{}`",
        display_resolution(&subject.resolution, registry),
    );
    let hint = if is_global_generic(&subject.resolution, "Option", registry) {
        "name the error first: `.or_err(error)` turns an `Option` into a `Result`"
    } else {
        "only a fallible expression (one producing a `Result`) can go here"
    };
    Diagnostic::error_with_hint(message, hint, subject.span)
}

fn is_global_generic(ty: &ResolvedType, name: &str, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = peel_alias(ty, registry)
    else {
        return false;
    };
    is_global_named(id, name, registry)
}

/// The shared `Ok(binder) -> binder` unwrap arm of both desugars.
fn unwrap_arm(binder: &str, span: Span) -> MatchArm {
    variant_arm(
        "Ok",
        binding_pattern(binder, span),
        ident_expr(binder, span),
        span,
    )
}

/// A `<variant>(element) -> tail` match arm. The constructor
/// shorthand pattern resolves the variant against the subject's
/// enum, exactly as a user-written `Ok(x)` arm would.
fn variant_arm(variant: &str, element: Pattern, tail: Expr, span: Span) -> MatchArm {
    MatchArm {
        pattern: Pattern::Constructor {
            name: variant.to_string(),
            elements: vec![element],
            span,
        },
        guard: None,
        body: vec![Statement::Expr(tail)],
        span,
    }
}

fn binding_pattern(name: &str, span: Span) -> Pattern {
    Pattern::Binding {
        local_id: None,
        name: name.to_string(),
        span,
    }
}

fn ident_expr(name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    )
}
