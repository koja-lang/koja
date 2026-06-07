//! Shared "literal-protocol" carrier machinery.
//!
//! Each protocol-aware literal family (today: `[a, b, c]` →
//! `ListLiteral<T>`, `["k": v, ...]` → `MapLiteral<K, V>`; soon:
//! `123` → `IntLiteral<T>`, `1.0` → `FloatLiteral<T>`, `<<...>>` →
//! `BinaryLiteral<T>`) shares the same carrier-rewrite shape:
//!
//! - The literal has a *default* canonical type (e.g. `List<T>`).
//!   That's what the literal "really is" — the value flowing out of
//!   the literal node is always an instance of the default carrier.
//! - When the surrounding context expects some *other* conformer
//!   `X` of the literal's protocol, the AST is rewritten in-place
//!   into `X.<from_method>(<canonical-literal>)` and the inner
//!   literal keeps its canonical-default type. The synthesized
//!   method call is dispatched through the normal method-call
//!   resolver so generic inference + arity checks all flow the
//!   same way.
//!
//! [`CarrierSpec`] captures the protocol's identifying strings;
//! [`pick_carrier`] decides which branch fires based on the
//! expected-type hint; [`dispatch_via_carrier`] performs the
//! AST-rewrite-or-restore step. Per-shape resolvers
//! ([`super::list`], [`super::map`]) plug their literal-specific
//! axis-inference into this spine.
//!
//! Protocol id resolution is registry-backed: a missing
//! `Global.<protocol_name>` autoimport silently falls through to
//! the default carrier. That's the only graceful-degradation point
//! — a missing default-carrier autoimport is a hard diagnostic.

use koja_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::super::calls::resolve_method_call_expr;
use super::super::ctx::Resolver;

/// Per-protocol metadata. Drives diagnostic phrasing,
/// registry lookups, and the synthesis-method name. Kept as a
/// `'static`-friendly struct (no lifetimes) so each literal
/// resolver can hold its `CarrierSpec` in a `const`.
pub(super) struct CarrierSpec {
    /// `Global.<root_name>` is the literal's default carrier
    /// (e.g. `List`, `Map`, `Int`, `Float`, `Binary`).
    pub root_name: &'static str,

    /// `Global.<protocol_name>` is the protocol the carrier
    /// dispatch keys on (e.g. `ListLiteral`, `MapLiteral`,
    /// `IntLiteral`).
    pub protocol_name: &'static str,

    /// The synthesis method on a non-default conformer. Has
    /// signature `fn <from_method>(<param>: <DefaultCarrier>) -> Self`.
    pub from_method: &'static str,

    /// Diagnostic prefix for the "default carrier missing from
    /// registry" error. Renders as "{label} requires
    /// `Global.<root_name>` to be autoimported".
    pub missing_root_label: &'static str,
}

/// Result of carrier selection. `Default` is the trivial-identity
/// case where the literal stays its canonical shape and resolves
/// to the default carrier instantiated at the inferred axes.
/// `Other` carries everything the synthesis path needs to mint a
/// `MethodCall` receiver (`<name>` ident at `<ident_span>`).
pub(super) enum LiteralCarrier {
    Default,
    Other { name: String, ident_span: Span },
}

/// Read a `Global.<name>` registry id, or `None` if the autoimport
/// is missing. Shared between the default-carrier lookup and the
/// protocol-id lookup since both speak the same `Global.<name>`
/// shape.
pub(super) fn lookup_global_id(resolver: &Resolver<'_>, name: &str) -> Option<GlobalRegistryId> {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    resolver.registry.lookup(&ident).map(|(id, _)| id)
}

/// Pick the carrier from the surrounding hint. The default carrier
/// always wins when the hint is missing, points at the default
/// carrier itself, or names a non-conforming type. A hint that
/// points at a registered struct/enum which conforms to
/// `Global.<protocol_name>` becomes the synthesized-`from_method`
/// carrier.
pub(super) fn pick_carrier(
    expected: Option<&ResolvedType>,
    default_id: GlobalRegistryId,
    spec: &CarrierSpec,
    resolver: &Resolver<'_>,
) -> LiteralCarrier {
    let Some(ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    }) = expected
    else {
        return LiteralCarrier::Default;
    };
    if *id == default_id {
        return LiteralCarrier::Default;
    }
    let Some(protocol_id) = lookup_global_id(resolver, spec.protocol_name) else {
        return LiteralCarrier::Default;
    };
    if resolver
        .registry
        .lookup_conformance(*id, protocol_id)
        .is_none()
    {
        return LiteralCarrier::Default;
    }
    let entry = match resolver.registry.get(*id) {
        Some(entry) => entry,
        None => return LiteralCarrier::Default,
    };
    LiteralCarrier::Other {
        name: entry.identifier.last().to_string(),
        ident_span: entry.span,
    }
}

/// Data needed to dispatch a literal through a carrier. Bundles
/// the expected type, carrier selection result, and protocol spec.
pub(super) struct Dispatch<'a> {
    pub expected: Option<&'a ResolvedType>,
    pub carrier: LiteralCarrier,
    pub spec: &'a CarrierSpec,
}

/// Drop the canonical inner literal back into `expr` (for the
/// default carrier) or synthesize a `<conformer>.<from_method>(<canonical>)`
/// MethodCall on `expr` and resolve it (for a non-default
/// conformer). Either way the returned [`ResolvedType`] is what
/// the caller stamps on `expr.resolution`.
///
/// `inner_kind` / `inner_resolution` describe the canonical inner
/// literal (e.g. `ExprKind::List { ... }` with resolution
/// `List<T>`). The caller has already inferred the axis types and
/// built both halves; this function just decides where they land.
pub(super) fn dispatch_via_carrier(
    expr: &mut Expr,
    inner_kind: ExprKind,
    inner_resolution: ResolvedType,
    ctx: &Dispatch<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let span = expr.span;
    let Dispatch {
        expected,
        carrier,
        spec,
    } = ctx;
    match carrier {
        LiteralCarrier::Default => {
            expr.kind = inner_kind;
            inner_resolution
        }
        LiteralCarrier::Other { name, ident_span } => {
            let mut inner = Expr::new(inner_kind, span);
            inner.resolution = inner_resolution;
            let receiver = Expr::new(
                ExprKind::Ident {
                    name: name.clone(),
                    resolution: Resolution::Unresolved,
                },
                *ident_span,
            );
            expr.kind = ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method: spec.from_method.to_string(),
                args: vec![Arg {
                    name: None,
                    span,
                    value: inner,
                }],
                type_args: Vec::new(),
            };
            // The synthesized call dispatches through the normal
            // method-call resolver: it populates the receiver's
            // resolution (`Global(id)` leaf, then upgraded with
            // inferred type-args), validates the arg shape against
            // `<from_method>(<param>: <DefaultCarrier>) -> Self`,
            // and returns the substituted `Self` (the carrier with
            // its slots filled in).
            resolve_method_call_expr(expr, *expected, resolver, diagnostics)
        }
    }
}

/// Diagnostic helper: emit "{spec.missing_root_label} requires
/// `Global.<root_name>` to be autoimported" at `span`. Returns
/// [`ResolvedType::unresolved`] so the caller can plumb the
/// short-circuit through its own control flow.
pub(super) fn missing_root_diagnostic(
    spec: &CarrierSpec,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    diagnostics.push(Diagnostic::error(
        format!(
            "{} requires `Global.{}` to be autoimported",
            spec.missing_root_label, spec.root_name,
        ),
        span,
    ));
    ResolvedType::unresolved()
}
