//! `["k1": v1, "k2": v2]` resolution. The surrounding hint chooses
//! which `MapLiteral<K, V>` conformer receives the entries.
//!
//! - No hint, or hint is `Map<K, V>`: the literal stays
//!   [`ExprKind::Map`] on the sealed AST and stamps
//!   `expr.resolution = Map<K, V>`. IR lowering builds the map
//!   directly without an entry-list allocation.
//! - Hint is some `X` that has an `impl MapLiteral<K, V> for X` in
//!   the registry: the outer expression is rewritten in-place into
//!   a synthesized `X.from_entries([("k", v), ...])` method call.
//!   The ordered entry list preserves source order and duplicate keys.
//!
//! Carrier mechanics live in [`super::carrier`]. This file only
//! owns map-literal-specific work (entry-take, key/value-type
//! inference).

use koja_ast::ast::{Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::axis::{AxisLabel, infer_axis};
use super::carrier::{
    CarrierSpec, Dispatch, LiteralCarrier, dispatch_via_carrier, lookup_global_id,
    missing_root_diagnostic, pick_carrier,
};

const SPEC: CarrierSpec = CarrierSpec {
    root_name: "Map",
    protocol_name: "MapLiteral",
    from_method: "from_entries",
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
    let (key_hint, value_hint) = entry_hints(&carrier, expected);

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
        type_args: vec![key_ty.clone(), value_ty.clone()],
    };

    if matches!(&carrier, LiteralCarrier::Default) {
        expr.kind = ExprKind::Map { entries };
        return map_ty;
    }

    let Some(list_id) = lookup_global_id(resolver, "List") else {
        diagnostics.push(Diagnostic::error(
            "map literal carriers require `Global.List` to be autoimported",
            span,
        ));
        expr.kind = ExprKind::Map { entries };
        return ResolvedType::unresolved();
    };
    let tuple_ty = ResolvedType::Anonymous(AnonymousKind::Tuple {
        elements: vec![key_ty, value_ty],
    });
    let entry_exprs = entries
        .into_iter()
        .map(|(key, value)| {
            let mut entry = Expr::new(
                ExprKind::Tuple {
                    elements: vec![key, value],
                },
                span,
            );
            entry.resolution = tuple_ty.clone();
            entry
        })
        .collect();
    let entries_ty = ResolvedType::Named {
        resolution: Resolution::Global(list_id),
        type_args: vec![tuple_ty],
    };

    dispatch_via_carrier(
        expr,
        ExprKind::List {
            elements: entry_exprs,
        },
        entries_ty,
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
fn entry_hints(
    carrier: &LiteralCarrier,
    expected: Option<&ResolvedType>,
) -> (Option<ResolvedType>, Option<ResolvedType>) {
    let type_args = match carrier.protocol_args() {
        Some(args) => args,
        None => {
            let Some(ResolvedType::Named { type_args, .. }) = expected else {
                return (None, None);
            };
            type_args
        }
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
            "resolve_map_literal was called with non-Map ExprKind {}",
            koja_ast::labels::expr_kind_label(&other)
        ),
    }
}
