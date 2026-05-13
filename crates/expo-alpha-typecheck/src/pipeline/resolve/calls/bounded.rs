//! Bounded method-call resolution: `t.method()` where `t`'s static
//! type is a generic type-parameter `T` whose bounds list provides
//! the method.
//!
//! Walks the bound's protocols, finds the unique provider (or
//! emits not-found / ambiguity), validates args against the
//! protocol method's signature with `Self → t`, and returns the
//! substituted return type. The receiver's `Resolution::TypeParam`
//! stays put; IR-side substitution rewrites it into a concrete
//! type post-mono and the regular `[concrete_target, method_name]`
//! lookup picks up the impl method.

use expo_ast::ast::{Arg, Diagnostic, Expr};
use expo_ast::coercion::{Coercion, LiteralCoercion};
use expo_ast::identifier::{GlobalRegistryId, ResolvedType, TypeParamIndex};
use expo_ast::span::Span;

use crate::registry::{Dispatch, GlobalKind, GlobalRegistry, ResolvedProtocolMethod};

use super::super::coercion::{
    Compatible, check_compatible, coercion_annotation_mut, coercion_target_mut,
};
use super::super::ctx::Resolver;
use super::super::types::display_resolution;

/// Inputs to [`resolve_bounded_method_call`]. Bundles every
/// `recv.method(args)` shard so the helper stays under
/// `too_many_arguments` and reads as a structured site rather than
/// a positional argument soup.
pub(super) struct BoundedCall<'a> {
    pub(super) args: &'a mut [Arg],
    pub(super) call_span: Span,
    pub(super) index: TypeParamIndex,
    pub(super) method: &'a str,
    pub(super) owner: GlobalRegistryId,
    pub(super) receiver: &'a Expr,
}

pub(super) fn resolve_bounded_method_call(
    site: BoundedCall<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let BoundedCall {
        receiver,
        owner,
        index,
        method,
        args,
        call_span,
    } = site;
    let declared_bounds: Vec<GlobalRegistryId> = resolver
        .registry
        .type_param_bounds(owner)
        .and_then(|all| all.get(index.as_u32() as usize))
        .cloned()
        .unwrap_or_default();
    let param_name = resolver
        .registry
        .type_param_name(owner, index)
        .unwrap_or("?")
        .to_string();
    let bounds = effective_bounds(&declared_bounds, resolver.registry);
    if bounds.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!("no method `{method}` on type parameter `{param_name}` (no bounds declared)",),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    let providers = collect_bound_providers(&bounds, method, resolver.registry);
    if providers.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "no method `{method}` on type parameter `{param_name}` \
                 (no bound provides it)",
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    if providers.len() > 1 {
        let labels: Vec<String> = providers
            .iter()
            .map(|(id, _)| {
                resolver
                    .registry
                    .get(*id)
                    .map(|e| e.identifier.last().to_string())
                    .unwrap_or_else(|| format!("<id {id}>"))
            })
            .collect();
        diagnostics.push(Diagnostic::error(
            format!(
                "ambiguous method `{method}` on type parameter `{param_name}` \
                 — provided by both `{}` and `{}` in bounds",
                labels[0], labels[1],
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    let (_, protocol_method) = providers.into_iter().next().expect("len == 1");
    if protocol_method.dispatch != Dispatch::Instance {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot call static method `{method}` of bound protocol on a value of \
                 type parameter `{param_name}` — use the protocol name to dispatch",
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    validate_bounded_args(
        BoundedArgsSite {
            method,
            param_name: &param_name,
            args,
            protocol_method: &protocol_method,
            call_span,
        },
        resolver,
        diagnostics,
    );
    // Return type may carry `Self` (TypeParam at protocol's slot 0).
    // Generic protocols (slice 2.7+) will additionally substitute the
    // protocol's user-declared params against the receiver's type-args
    // — currently the protocol-method scope is `Self`-only so the
    // return type passes through unchanged.
    let _ = receiver;
    protocol_method.return_type
}

/// Augment a type-parameter's declared bounds with the universal
/// protocols ([`crate::registry::UNIVERSAL_PROTOCOLS`]) so callers
/// like `T.format()` resolve on bare type parameters without an
/// explicit `T: Debug` annotation. The synthesizer / hand-written
/// stdlib impls guarantee every concrete monomorphization carries a
/// `Debug` impl, so the universal fallback is sound after
/// monomorphization.
///
/// Universal ids are appended in [`crate::registry::UNIVERSAL_PROTOCOLS`]
/// order, deduped against any duplicate the user already declared.
fn effective_bounds(
    declared: &[GlobalRegistryId],
    registry: &GlobalRegistry,
) -> Vec<GlobalRegistryId> {
    let mut bounds = declared.to_vec();
    for id in registry.universal_protocol_ids() {
        if !bounds.contains(&id) {
            bounds.push(id);
        }
    }
    bounds
}

/// Walk a type-param's bound list and collect every protocol that
/// declares a method named `method`. Returns clones so the caller
/// can drop the registry borrow before validating arg shapes.
fn collect_bound_providers(
    bounds: &[GlobalRegistryId],
    method: &str,
    registry: &GlobalRegistry,
) -> Vec<(GlobalRegistryId, ResolvedProtocolMethod)> {
    let mut providers = Vec::new();
    for &protocol_id in bounds {
        let Some(entry) = registry.get(protocol_id) else {
            continue;
        };
        let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
            continue;
        };
        if let Some(found) = definition.methods.iter().find(|m| m.name == method) {
            providers.push((protocol_id, found.clone()));
        }
    }
    providers
}

/// Inputs to [`validate_bounded_args`]. Bundled so the helper
/// stays under `too_many_arguments` while still surfacing the
/// per-call site fields a bounded protocol-method dispatch needs:
/// the user-facing labels (`method` / `param_name`), the supplied
/// args, the resolved protocol method's signature, and the call
/// expression's source span. Mirrors [`BoundedCall`]'s shape.
pub(super) struct BoundedArgsSite<'a> {
    pub(super) args: &'a mut [Arg],
    pub(super) call_span: Span,
    pub(super) method: &'a str,
    pub(super) param_name: &'a str,
    pub(super) protocol_method: &'a ResolvedProtocolMethod,
}

/// Check arity + per-position type compatibility for a bounded
/// method call. Mirrors [`super::validate_arg_signature`]'s wording so
/// a "wrong arg type" diagnostic reads identically whether the
/// call dispatches against a struct method or a protocol method.
fn validate_bounded_args(
    site: BoundedArgsSite<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let BoundedArgsSite {
        method,
        param_name,
        args,
        protocol_method,
        call_span,
    } = site;
    if args.len() != protocol_method.non_self_params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "method `{method}` on `{param_name}` expects {} argument{}, got {}",
                protocol_method.non_self_params.len(),
                if protocol_method.non_self_params.len() == 1 {
                    ""
                } else {
                    "s"
                },
                args.len(),
            ),
            call_span,
        ));
        return;
    }
    for (arg, expected) in args.iter_mut().zip(protocol_method.non_self_params.iter()) {
        let actual = arg.value.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible(&arg.value, &actual, &expected.ty, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                *coercion_target_mut(&mut arg.value) =
                    Some(LiteralCoercion::NumericLiteralWidth(width));
            }
            Compatible::UnionWiden { target } => {
                *coercion_annotation_mut(&mut arg.value) = Some(Coercion::UnionWiden(target));
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{method}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        expected.name,
                        display_resolution(&expected.ty, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    arg.span,
                ));
            }
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{method}` expects `{}`, got `{}`",
                        expected.name,
                        display_resolution(&expected.ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}
