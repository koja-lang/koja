//! Closure expression resolution.
//!
//! Resolves both block (`fn x -> body end`) and short (`x -> body`)
//! closure forms. Each closure body opens a snapshotted scope inside
//! the enclosing function's [`LocalScope`]: closure params are
//! [`LocalScope::declare`]d (shadowing outer names while the body
//! resolves) and the snapshot reverts the name table on exit. The
//! shared [`LocalId`] counter keeps every binding's id unique across
//! the function so IR-level capture analysis can derive captures
//! purely from `LocalRead` ids minted before vs after the closure
//! entered scope.
//!
//! Captured idents inside the body resolve to [`Resolution::Local`]
//! pointing at the outer scope's [`LocalId`] — no special "capture"
//! marker — and the type table makes that id's
//! [`ResolvedType`] visible everywhere the body is later walked.
//!
//! Param types come from the explicit annotation when present,
//! otherwise from an `expected: fn (T0, T1) -> R` shape supplied by
//! the surrounding context (the call/method-call resolvers thread
//! this from the corresponding parameter's signature). Missing both
//! sources is an error: the closure resolves with [`Unresolved`]
//! params and the body still walks for diagnostic completeness.
//!
//! [`LocalScope::declare`]: crate::pipeline::local_scope::LocalScope::declare
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`ResolvedType`]: koja_ast::identifier::ResolvedType
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local
//! [`Unresolved`]: koja_ast::identifier::ResolvedType::Unresolved

use koja_ast::ast::{ClosureParam, Diagnostic, Expr, Statement, TypeExpr};
use koja_ast::identifier::{AnonymousKind, ResolvedType};
use koja_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr_with_expected;
use super::walker::resolve_body_with_expected;
use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};

/// Resolve a block closure (`fn (x: Int) -> Int ... end` or
/// `fn x -> body end`). Stamps `local_id` on each named param,
/// resolves the body under a snapshot, and returns the
/// [`AnonymousKind::Function`] type.
pub(super) fn resolve_closure(
    params: &mut [ClosureParam],
    return_type: &Option<TypeExpr>,
    body: &mut [Statement],
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let expected_params = expected_function_params(expected);
    let expected_return = expected_function_return(expected);
    let annotated_return = resolve_return_annotation(return_type, resolver, diagnostics);
    let resolved_params = bind_closure_params(params, expected_params, span, resolver, diagnostics);
    let snapshot = resolver.scope.snapshot();
    declare_closure_params(params, &resolved_params, resolver);

    let return_hint = annotated_return
        .clone()
        .or_else(|| expected_return.cloned());
    let saved_return = std::mem::replace(&mut resolver.current_return_type, return_hint.clone());
    let saved_loop_depth = std::mem::replace(&mut resolver.loop_depth, 0);
    let saved_loop_break_seen = std::mem::take(&mut resolver.loop_break_seen);
    // Thread the closure's annotated / context-derived return type
    // as the trailing-expression expected-type hint, mirroring how
    // [`crate::pipeline::resolve::walker::resolve_function_body`]
    // does for named functions. Without this, a trailing
    // `Result.Ok(v * 3)` in a closure annotated `-> Result<Int, Int>`
    // can't pin `E` from context and fires "cannot infer type
    // parameter `E` of `Global.Result`".
    resolve_body_with_expected(body, return_hint.as_ref(), resolver, diagnostics);
    let body_return_ty = trailing_expr_type(body);
    resolver.loop_break_seen = saved_loop_break_seen;
    resolver.loop_depth = saved_loop_depth;
    resolver.current_return_type = saved_return;
    resolver.scope.restore(snapshot);

    let ret = closure_return_type(annotated_return, body_return_ty, expected_return);
    ResolvedType::Anonymous(AnonymousKind::Function {
        params: resolved_params,
        ret: Box::new(ret),
    })
}

/// Resolve a short closure (`x -> body_expr`). Single-expression
/// body; otherwise mirrors [`resolve_closure`].
pub(super) fn resolve_short_closure(
    params: &mut [ClosureParam],
    body: &mut Expr,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let expected_params = expected_function_params(expected);
    let expected_return = expected_function_return(expected);
    let resolved_params = bind_closure_params(params, expected_params, span, resolver, diagnostics);
    let snapshot = resolver.scope.snapshot();
    declare_closure_params(params, &resolved_params, resolver);

    let saved_return =
        std::mem::replace(&mut resolver.current_return_type, expected_return.cloned());
    let saved_loop_depth = std::mem::replace(&mut resolver.loop_depth, 0);
    let saved_loop_break_seen = std::mem::take(&mut resolver.loop_break_seen);
    resolve_expr_with_expected(body, expected_return, resolver, diagnostics);
    let body_return_ty = body.resolution.clone();
    resolver.loop_break_seen = saved_loop_break_seen;
    resolver.loop_depth = saved_loop_depth;
    resolver.current_return_type = saved_return;
    resolver.scope.restore(snapshot);

    let ret = closure_return_type(None, Some(body_return_ty), expected_return);
    ResolvedType::Anonymous(AnonymousKind::Function {
        params: resolved_params,
        ret: Box::new(ret),
    })
}

/// Compute each closure param's declared type. Annotation wins over
/// context; both missing diagnoses and substitutes
/// [`ResolvedType::Unresolved`].
fn bind_closure_params(
    params: &[ClosureParam],
    expected_params: Option<&[ResolvedType]>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ResolvedType> {
    if let Some(expected) = expected_params
        && expected.len() != params.len()
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "closure expects {} parameter{}, got {}",
                expected.len(),
                if expected.len() == 1 { "" } else { "s" },
                params.len(),
            ),
            span,
        ));
    }
    params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let context = expected_params.and_then(|p| p.get(index));
            resolve_closure_param(param, context, resolver, diagnostics)
        })
        .collect()
}

fn resolve_closure_param(
    param: &ClosureParam,
    expected: Option<&ResolvedType>,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    match param {
        ClosureParam::Name {
            type_expr: Some(type_expr),
            ..
        } => resolve_type_expr(
            type_expr,
            TypeParamScope::default(),
            resolver.resolution_scope(),
            diagnostics,
        ),
        ClosureParam::Name {
            name,
            span,
            type_expr: None,
            ..
        } => match expected {
            Some(expected_ty) => expected_ty.clone(),
            None => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "closure parameter `{name}` needs a type annotation \
                         (no surrounding context to infer from)",
                    ),
                    *span,
                ));
                ResolvedType::unresolved()
            }
        },
        ClosureParam::Wildcard { .. } => expected.cloned().unwrap_or_else(ResolvedType::unresolved),
    }
}

/// Stamp `local_id` on each `Name` param after resolving its type so
/// the body can resolve `Resolution::Local` references to the right
/// ids.
fn declare_closure_params(
    params: &mut [ClosureParam],
    resolved: &[ResolvedType],
    resolver: &mut Resolver<'_>,
) {
    for (param, param_ty) in params.iter_mut().zip(resolved.iter()) {
        match param {
            ClosureParam::Name { local_id, name, .. } => {
                let id = resolver.scope.declare(name, param_ty.clone());
                *local_id = Some(id);
            }
            ClosureParam::Wildcard { local_id, .. } => {
                let id = resolver.scope.declare_anonymous(param_ty.clone());
                *local_id = Some(id);
            }
        }
    }
}

/// Pick the closure's return type. Explicit annotation wins; else
/// the body's trailing expression; else the expected return from
/// context; else [`ResolvedType::Unresolved`].
fn closure_return_type(
    annotation: Option<ResolvedType>,
    body_return: Option<ResolvedType>,
    expected: Option<&ResolvedType>,
) -> ResolvedType {
    if let Some(ty) = annotation {
        return ty;
    }
    if let Some(ty) = body_return
        && ty.is_resolved()
    {
        return ty;
    }
    expected.cloned().unwrap_or_else(ResolvedType::unresolved)
}

/// Resolve the closure's `-> T` annotation eagerly so it can both
/// seed `current_return_type` for body resolution and fall through
/// as the closure's published return type.
fn resolve_return_annotation(
    annotation: &Option<TypeExpr>,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let type_expr = annotation.as_ref()?;
    Some(resolve_type_expr(
        type_expr,
        TypeParamScope::default(),
        resolver.resolution_scope(),
        diagnostics,
    ))
}

fn trailing_expr_type(body: &[Statement]) -> Option<ResolvedType> {
    body.last().and_then(|stmt| match stmt {
        Statement::Expr(expr) => Some(expr.resolution.clone()),
        _ => None,
    })
}

fn expected_function_params(expected: Option<&ResolvedType>) -> Option<&[ResolvedType]> {
    match expected {
        Some(ResolvedType::Anonymous(AnonymousKind::Function { params, .. })) => Some(params),
        _ => None,
    }
}

fn expected_function_return(expected: Option<&ResolvedType>) -> Option<&ResolvedType> {
    match expected {
        Some(ResolvedType::Anonymous(AnonymousKind::Function { ret, .. })) => Some(ret),
        _ => None,
    }
}
