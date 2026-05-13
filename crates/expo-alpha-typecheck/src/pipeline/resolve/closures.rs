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
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`ResolvedType`]: expo_ast::identifier::ResolvedType
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local
//! [`Unresolved`]: expo_ast::identifier::ResolvedType::Unresolved

use expo_ast::ast::{ClosureParam, Diagnostic, Expr, PassMode, Statement, TypeExpr};
use expo_ast::identifier::{AnonymousKind, FnParam, ResolvedType};
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr_with_expected;
use super::walker::resolve_statement;
use crate::pipeline::lift_signatures::{TypeParamScope, resolve_type_expr};

/// Resolve a block closure (`fn (x: Int) -> Int ... end` or
/// `fn x -> body end`). Stamps `local_id` on each named param,
/// resolves the body under a snapshot, and returns the
/// [`AnonymousKind::Function`] type with per-param mode preserved.
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
    let saved_return = std::mem::replace(&mut resolver.current_return_type, return_hint);
    for stmt in body.iter_mut() {
        resolve_statement(stmt, resolver, diagnostics);
    }
    let body_return_ty = trailing_expr_type(body);
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
    resolve_expr_with_expected(body, expected_return, resolver, diagnostics);
    let body_return_ty = body.resolution.clone();
    resolver.current_return_type = saved_return;
    resolver.scope.restore(snapshot);

    let ret = closure_return_type(None, Some(body_return_ty), expected_return);
    ResolvedType::Anonymous(AnonymousKind::Function {
        params: resolved_params,
        ret: Box::new(ret),
    })
}

/// Compute each closure param's declared [`FnParam`] (mode + type).
/// Annotation wins over context; both missing diagnoses and
/// substitutes [`ResolvedType::Unresolved`].
fn bind_closure_params(
    params: &[ClosureParam],
    expected_params: Option<&[FnParam]>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<FnParam> {
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
    expected: Option<&FnParam>,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> FnParam {
    match param {
        ClosureParam::Name {
            mode,
            type_expr: Some(type_expr),
            ..
        } => FnParam {
            mode: *mode,
            ty: resolve_type_expr(
                type_expr,
                TypeParamScope::default(),
                resolver.resolution_scope(),
                diagnostics,
            ),
        },
        ClosureParam::Name {
            mode,
            name,
            span,
            type_expr: None,
            ..
        } => match expected {
            Some(expected_param) => FnParam {
                mode: *mode,
                ty: expected_param.ty.clone(),
            },
            None => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "closure parameter `{name}` needs a type annotation \
                         (no surrounding context to infer from)",
                    ),
                    *span,
                ));
                FnParam {
                    mode: *mode,
                    ty: ResolvedType::unresolved(),
                }
            }
        },
        ClosureParam::Wildcard { .. } => FnParam {
            mode: PassMode::Borrow,
            ty: expected
                .map(|p| p.ty.clone())
                .unwrap_or_else(ResolvedType::unresolved),
        },
        ClosureParam::Destructured { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support destructured closure parameters".to_string(),
                *span,
            ));
            FnParam {
                mode: PassMode::Borrow,
                ty: ResolvedType::unresolved(),
            }
        }
    }
}

/// Stamp `local_id` on each `Name` param after resolving its type so
/// the body can resolve `Resolution::Local` references to the right
/// ids.
fn declare_closure_params(
    params: &mut [ClosureParam],
    resolved: &[FnParam],
    resolver: &mut Resolver<'_>,
) {
    for (param, fn_param) in params.iter_mut().zip(resolved.iter()) {
        if let ClosureParam::Name { local_id, name, .. } = param {
            let id = resolver.scope.declare(name, fn_param.ty.clone());
            *local_id = Some(id);
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

fn expected_function_params(expected: Option<&ResolvedType>) -> Option<&[FnParam]> {
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
