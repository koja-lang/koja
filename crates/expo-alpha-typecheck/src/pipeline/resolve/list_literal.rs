//! `[a, b, c]` resolution. Walks the elements with the surrounding
//! `List<T>` hint (when one is in scope), agrees on the element type,
//! and stamps `expr.resolution = List<T>`. The literal stays as
//! [`ExprKind::List`] on the sealed AST — IR lower turns it into the
//! `List.new().append(...)` chain at codegen time.

use expo_ast::ast::{Diagnostic, Expr};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr_with_expected;
use super::types::display_resolution;

pub(super) fn resolve_list_literal(
    elements: &mut [Expr],
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(list_id) = lookup_list_id(resolver) else {
        diagnostics.push(Diagnostic::error(
            "list literal `[...]` requires `Global.List` to be autoimported".to_string(),
            span,
        ));
        return ResolvedType::unresolved();
    };

    let element_hint = expected_element_type(expected, list_id);
    for element in elements.iter_mut() {
        resolve_expr_with_expected(element, element_hint.as_ref(), resolver, diagnostics);
    }

    let Some(element_ty) =
        infer_element_type(elements, element_hint.as_ref(), span, resolver, diagnostics)
    else {
        return ResolvedType::unresolved();
    };

    ResolvedType::Named {
        resolution: Resolution::Global(list_id),
        type_args: vec![element_ty],
    }
}

/// Read `Global.List` out of the registry. Returns `None` only when
/// the stdlib autoimport is missing — every alpha entry point pulls
/// in `list.expo`, so this is effectively an availability check.
fn lookup_list_id(resolver: &Resolver<'_>) -> Option<GlobalRegistryId> {
    let ident = Identifier::new("Global", vec!["List".to_string()]);
    resolver.registry.lookup(&ident).map(|(id, _)| id)
}

/// Pull the element type out of an expected `List<X>`. Returns
/// `None` when there's no hint, when the hint isn't a `List`, or
/// when the hint's element slot isn't fully resolved.
fn expected_element_type(
    expected: Option<&ResolvedType>,
    list_id: GlobalRegistryId,
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
