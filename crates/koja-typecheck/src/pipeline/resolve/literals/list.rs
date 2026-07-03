//! `[a, b, c]` resolution. The literal's *value type* is always
//! `List<T>`. The surrounding context picks which
//! `ListLiteral<T>` conformer the value flows into:
//!
//! - No hint, or hint is `List<T>`: the literal stays
//!   [`ExprKind::List`] on the sealed AST and stamps
//!   `expr.resolution = List<T>`. (`List<T>` itself implements
//!   `ListLiteral<T>` as the trivial identity, so no `from_list`
//!   wrap is needed.)
//! - Hint is some `X<T>` that has an `impl ListLiteral<T> for X<T>`
//!   in the registry: the outer expression is rewritten in-place
//!   into a synthesized `X.from_list([a, b, c])` method call. The
//!   inner literal keeps `ExprKind::List` and stamps `List<T>`,
//!   the outer rewritten node stamps `X<T>` and dispatches through
//!   the normal method-call resolver.
//!
//! This keeps IR lower a pure translator: it only ever sees a
//! `ExprKind::List` whose resolution is `List<T>`. The carrier
//! mechanics live in [`super::carrier`]. This file only owns
//! list-literal-specific work (axis-take, element-type inference).

use koja_ast::ast::{Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{Resolution, ResolvedType};

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::axis::{AxisLabel, infer_axis};
use super::carrier::{
    CarrierSpec, Dispatch, dispatch_via_carrier, lookup_global_id, missing_root_diagnostic,
    pick_carrier,
};

const SPEC: CarrierSpec = CarrierSpec {
    root_name: "List",
    protocol_name: "ListLiteral",
    from_method: "from_list",
    missing_root_label: "list literal `[...]`",
};

const AXIS: AxisLabel<'static> = AxisLabel {
    collection: "list literal",
    axis: "element",
};

const EMPTY_EXAMPLE: &str = "result: List<Int> = []";

pub(in super::super) fn resolve_list_literal(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Short-circuit when the literal has already been resolved. The
    // `from_list` synthesis stamps `inner.resolution = List<T>` on
    // the inner literal before handing the synthesized `MethodCall`
    // to the method-call resolver. Arg resolution then re-enters
    // this function with `expected = List<T>` from the `from_list`
    // signature, and the unbound `T` would drop the element-type
    // hint and re-diagnose an empty `[]` literal as having no
    // element type.
    if expr.resolution.is_resolved() {
        return expr.resolution.clone();
    }
    let span = expr.span;
    let Some(list_id) = lookup_global_id(resolver, SPEC.root_name) else {
        return missing_root_diagnostic(&SPEC, span, diagnostics);
    };

    let carrier = pick_carrier(expected, list_id, &SPEC, resolver);
    let element_hint = first_axis_hint(expected);

    let mut elements = take_elements(&mut expr.kind);
    for element in elements.iter_mut() {
        resolve_expr_with_expected(element, element_hint.as_ref(), resolver, diagnostics);
    }

    let Some(element_ty) = infer_axis(
        elements.iter(),
        element_hint.as_ref(),
        AXIS,
        span,
        EMPTY_EXAMPLE,
        resolver,
        diagnostics,
    ) else {
        // Restore so the AST shape stays consistent for the
        // diagnostic-report path. `expr.resolution` ends up
        // `Unresolved`, which seal never sees because diagnostics
        // are non-empty.
        expr.kind = ExprKind::List { elements };
        return ResolvedType::unresolved();
    };

    let list_ty = ResolvedType::Named {
        resolution: Resolution::Global(list_id),
        type_args: vec![element_ty],
    };

    dispatch_via_carrier(
        expr,
        ExprKind::List { elements },
        list_ty,
        &Dispatch {
            expected,
            carrier,
            spec: &SPEC,
        },
        resolver,
        diagnostics,
    )
}

/// Pull the first type-arg out of `expected` when both the
/// resolution and the slot are present and resolved. Used as the
/// element-type hint flowing into per-element resolution.
fn first_axis_hint(expected: Option<&ResolvedType>) -> Option<ResolvedType> {
    let ResolvedType::Named { type_args, .. } = expected? else {
        return None;
    };
    let element = type_args.first()?;
    element.is_resolved().then(|| element.clone())
}

/// Pull the elements vec out of `expr.kind` so the caller can
/// rebuild the kind into a different shape (or restore it). Panics
/// if `expr.kind` isn't `List`, but every call site has matched on
/// `ExprKind::List` already.
fn take_elements(kind: &mut ExprKind) -> Vec<Expr> {
    let stub = ExprKind::List {
        elements: Vec::new(),
    };
    match std::mem::replace(kind, stub) {
        ExprKind::List { elements } => elements,
        other => unreachable!(
            "resolve_list_literal called with non-List ExprKind: {}",
            koja_ast::labels::expr_kind_label(&other)
        ),
    }
}
