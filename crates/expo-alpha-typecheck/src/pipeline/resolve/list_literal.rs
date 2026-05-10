//! `[a, b, c]` rewrite — produces the desugared
//! `List.new().append(a).append(b).append(c)` chain with every
//! resolution stamped from the elements' inferred types. Runs at
//! resolve time (rather than the synth phase) because the
//! `T = element type` constraint only exists once the elements have
//! been resolved; constructing the chain before that point would
//! emit a "cannot infer `T`" diagnostic at `List.new()`.

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr_with_expected;
use super::types::display_resolution;

/// Resolve the elements of a list literal, agree on the element
/// type, and replace `expr_kind`/`expr_resolution` (via the
/// returned tuple) with the desugared `List.new().append(...)`
/// chain. Empty `[]` collapses to a bare `List.new()`.
pub(super) fn resolve_list_literal(
    elements: &mut Vec<Expr>,
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (ExprKind, ResolvedType) {
    let Some(list_id) = lookup_list_id(resolver) else {
        diagnostics.push(Diagnostic::error(
            "list literal `[...]` requires `Global.List` to be autoimported".to_string(),
            span,
        ));
        return unresolved_chain(span);
    };

    let element_hint = expected_element_type(expected, list_id);
    for element in elements.iter_mut() {
        resolve_expr_with_expected(element, element_hint.as_ref(), resolver, diagnostics);
    }

    let Some(element_ty) =
        infer_element_type(elements, element_hint.as_ref(), span, resolver, diagnostics)
    else {
        return unresolved_chain(span);
    };

    let list_ty = ResolvedType::Named {
        resolution: Resolution::Global(list_id),
        type_args: vec![element_ty.clone()],
    };
    let chain = build_chain(std::mem::take(elements), list_id, list_ty.clone(), span);
    (chain, list_ty)
}

/// Read `Global.List` out of the registry. Returns `None` only when
/// the stdlib autoimport is missing — every alpha entry point pulls
/// in `list.expo`, so this is effectively an availability check.
fn lookup_list_id(resolver: &Resolver<'_>) -> Option<expo_ast::identifier::GlobalRegistryId> {
    let ident = Identifier::new("Global", vec!["List".to_string()]);
    resolver.registry.lookup(&ident).map(|(id, _)| id)
}

/// Pull the element type out of an expected `List<X>`. Returns
/// `None` when there's no hint, when the hint isn't a `List`, or
/// when the hint's element slot isn't fully resolved.
fn expected_element_type(
    expected: Option<&ResolvedType>,
    list_id: expo_ast::identifier::GlobalRegistryId,
) -> Option<ResolvedType> {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        type_args,
    } = expected?
    else {
        return None;
    };
    if *id != list_id {
        return None;
    }
    let element = type_args.first()?;
    if element.is_resolved() {
        Some(element.clone())
    } else {
        None
    }
}

/// Pick the literal's element type. The hint (if any) wins; else
/// the first resolved element type sets the floor and every later
/// element is checked against it (mismatches diagnose). Empty
/// literals without a hint surface a "cannot infer element type"
/// diagnostic — the caller treats this as an unresolved literal.
fn infer_element_type(
    elements: &[Expr],
    hint: Option<&ResolvedType>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    if let Some(hint) = hint {
        for element in elements {
            if element.resolution.is_resolved() && &element.resolution != hint {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "list literal element type mismatch: expected `{}`, found `{}`",
                        display_resolution(hint, resolver.registry),
                        display_resolution(&element.resolution, resolver.registry),
                    ),
                    element.span,
                ));
            }
        }
        return Some(hint.clone());
    }

    let mut chosen: Option<ResolvedType> = None;
    for element in elements {
        if !element.resolution.is_resolved() {
            continue;
        }
        match &chosen {
            None => chosen = Some(element.resolution.clone()),
            Some(prev) if prev != &element.resolution => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "list literal element type mismatch: expected `{}`, found `{}`",
                        display_resolution(prev, resolver.registry),
                        display_resolution(&element.resolution, resolver.registry),
                    ),
                    element.span,
                ));
            }
            _ => {}
        }
    }

    if chosen.is_none() {
        diagnostics.push(Diagnostic::error(
            "list literal `[]` has no element type — annotate the binding or pass a context that \
             determines `T` (e.g. `result: List<Int> = []`)"
                .to_string(),
            span,
        ));
    }
    chosen
}

/// Fold `elements` left into the chain
/// `List.new().append(e₀).append(e₁)...`. Every receiver and outer
/// expression carries `list_ty` (`List<element>`), and the inner
/// `List` ident carries the bare `Global(list_id)` leaf so
/// downstream classify-receiver / IR-lower paths read the same
/// shape they'd see for hand-written `List.new()`.
fn build_chain(
    elements: Vec<Expr>,
    list_id: expo_ast::identifier::GlobalRegistryId,
    list_ty: ResolvedType,
    span: Span,
) -> ExprKind {
    let new_call = new_method_call(list_id, list_ty.clone(), span);
    let chain = elements.into_iter().fold(new_call, |receiver, element| {
        append_method_call(receiver, element, list_ty.clone(), span)
    });
    chain.kind
}

fn new_method_call(
    list_id: expo_ast::identifier::GlobalRegistryId,
    list_ty: ResolvedType,
    span: Span,
) -> Expr {
    // Receiver of `List.new()` mirrors the post-inference shape that
    // `resolve_method_call` stitches onto static-dispatch receivers:
    // the `Ident`'s own `resolution` is the bare `Global(list_id)`
    // leaf, but the surrounding `Expr.resolution` carries the fully
    // applied `List<element>` — IR lower pulls `type_args` straight
    // off the latter.
    let receiver = stamped_expr(
        ExprKind::Ident {
            name: "List".to_string(),
            resolution: Resolution::Global(list_id),
        },
        list_ty.clone(),
        span,
    );
    stamped_expr(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: "new".to_string(),
            args: Vec::new(),
            type_args: Vec::new(),
        },
        list_ty,
        span,
    )
}

fn append_method_call(receiver: Expr, element: Expr, list_ty: ResolvedType, span: Span) -> Expr {
    let arg_span = element.span;
    let args = vec![Arg {
        name: None,
        span: arg_span,
        value: element,
    }];
    stamped_expr(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: "append".to_string(),
            args,
            type_args: Vec::new(),
        },
        list_ty,
        span,
    )
}

fn stamped_expr(kind: ExprKind, resolution: ResolvedType, span: Span) -> Expr {
    let mut expr = Expr::new(kind, span);
    expr.resolution = resolution;
    expr
}

/// Fallback when the literal can't be desugared (missing `Global.List`
/// or the element type can't be inferred). Returns a stamped
/// `MethodCall { method: "new" }` so callers see the same outer
/// shape on success and failure; the unresolved leaf upstream
/// already triggered a diagnostic.
fn unresolved_chain(span: Span) -> (ExprKind, ResolvedType) {
    let receiver = Expr::new(
        ExprKind::Ident {
            name: "List".to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    );
    let kind = ExprKind::MethodCall {
        receiver: Box::new(receiver),
        method: "new".to_string(),
        args: Vec::new(),
        type_args: Vec::new(),
    };
    (kind, ResolvedType::unresolved())
}
