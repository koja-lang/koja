//! `["k1": v1, "k2": v2]` resolution. Mirrors [`super::list`]:
//! the literal's *value type* is always `Map<K, V>`, and the
//! surrounding hint chooses which `MapLiteral<K, V>` conformer
//! the value flows into.
//!
//! - No hint, or hint is `Map<K, V>`: the literal stays
//!   [`ExprKind::Map`] on the sealed AST and stamps
//!   `expr.resolution = Map<K, V>`. (`Map<K, V>` itself implements
//!   `MapLiteral<K, V>` as the trivial identity, so no `from_map`
//!   wrap is needed.)
//! - Hint is some `X<K, V>` that has an `impl MapLiteral<K, V> for
//!   X<K, V>` in the registry: the outer expression is rewritten
//!   in-place into a synthesized `X.from_map(["k": v, ...])`
//!   method call. The inner literal keeps `ExprKind::Map` and
//!   stamps `Map<K, V>`; the outer rewritten node stamps `X<K, V>`
//!   and dispatches through the normal method-call resolver.
//!
//! Carrier mechanics live in [`super::carrier`]; this file only
//! owns map-literal-specific work (entry-take, key/value-type
//! inference).

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
    root_name: "Map",
    protocol_name: "MapLiteral",
    from_method: "from_map",
    missing_root_label: "map literal `[k: v, ...]`",
};

const KEY_AXIS: AxisLabel<'static> = AxisLabel {
    collection: "map literal",
    axis: "key",
};

const VALUE_AXIS: AxisLabel<'static> = AxisLabel {
    collection: "map literal",
    axis: "value",
};

const KEY_EMPTY_EXAMPLE: &str = "result: Map<String, Int> = [\"a\": 1]";
const VALUE_EMPTY_EXAMPLE: &str = "result: Map<String, Int> = [\"a\": 1]";

pub(in super::super) fn resolve_map_literal(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // See [`super::list::resolve_list_literal`] for why the
    // already-resolved short-circuit is necessary.
    if expr.resolution.is_resolved() {
        return expr.resolution.clone();
    }
    let span = expr.span;
    let Some(map_id) = lookup_global_id(resolver, SPEC.root_name) else {
        return missing_root_diagnostic(&SPEC, span, diagnostics);
    };

    let carrier = pick_carrier(expected, map_id, &SPEC, resolver);
    let (key_hint, value_hint) = entry_hints(expected);

    let mut entries = take_entries(&mut expr.kind);
    for (key, value) in entries.iter_mut() {
        resolve_expr_with_expected(key, key_hint.as_ref(), resolver, diagnostics);
        resolve_expr_with_expected(value, value_hint.as_ref(), resolver, diagnostics);
    }

    let Some(key_ty) = infer_axis(
        entries.iter().map(|(k, _)| k),
        key_hint.as_ref(),
        KEY_AXIS,
        span,
        KEY_EMPTY_EXAMPLE,
        resolver,
        diagnostics,
    ) else {
        expr.kind = ExprKind::Map { entries };
        return ResolvedType::unresolved();
    };
    let Some(value_ty) = infer_axis(
        entries.iter().map(|(_, v)| v),
        value_hint.as_ref(),
        VALUE_AXIS,
        span,
        VALUE_EMPTY_EXAMPLE,
        resolver,
        diagnostics,
    ) else {
        expr.kind = ExprKind::Map { entries };
        return ResolvedType::unresolved();
    };

    let map_ty = ResolvedType::Named {
        resolution: Resolution::Global(map_id),
        type_args: vec![key_ty, value_ty],
    };

    dispatch_via_carrier(
        expr,
        ExprKind::Map { entries },
        map_ty,
        &Dispatch {
            expected,
            carrier,
            spec: &SPEC,
        },
        resolver,
        diagnostics,
    )
}

/// Pull `(K, V)` out of `expected.type_args[0..2]` when each slot
/// is fully resolved. Used as the per-axis hint flowing into
/// per-entry resolution.
fn entry_hints(expected: Option<&ResolvedType>) -> (Option<ResolvedType>, Option<ResolvedType>) {
    let Some(ResolvedType::Named { type_args, .. }) = expected else {
        return (None, None);
    };
    let key = type_args.first().filter(|t| t.is_resolved()).cloned();
    let value = type_args.get(1).filter(|t| t.is_resolved()).cloned();
    (key, value)
}

/// Pull the entries vec out of `expr.kind` so the caller can
/// rebuild the kind into a different shape (or restore it).
fn take_entries(kind: &mut ExprKind) -> Vec<(Expr, Expr)> {
    let stub = ExprKind::Map {
        entries: Vec::new(),
    };
    match std::mem::replace(kind, stub) {
        ExprKind::Map { entries } => entries,
        other => unreachable!(
            "resolve_map_literal called with non-Map ExprKind: {}",
            koja_ast::labels::expr_kind_label(&other)
        ),
    }
}
