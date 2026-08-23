//! Bounded method-call resolution: `t.method()` where `t`'s static
//! type is a generic type-parameter `T` whose bounds list provides
//! the method.
//!
//! Walks the bound's protocols, finds the unique provider (or
//! emits not-found / ambiguity), validates args against the
//! protocol method's signature with `Self -> t`, and returns the
//! substituted return type. The receiver's `Resolution::TypeParam`
//! stays put. IR-side substitution rewrites it into a concrete
//! type post-mono and the regular `[concrete_target, method_name]`
//! lookup picks up the impl method.

use koja_ast::ast::{Arg, Diagnostic, Expr};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex};
use koja_ast::span::Span;

use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{
    Dispatch, GlobalKind, GlobalRegistry, ResolvedProtocolBound, ResolvedProtocolMethod,
};

use super::super::coercion::{Mismatch, check_compatible_stamping};
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
    let mut declared_bounds: Vec<ResolvedProtocolBound> = resolver
        .registry
        .type_param_bounds(owner)
        .and_then(|all| all.get(index.as_u32() as usize))
        .cloned()
        .unwrap_or_default();
    // A conditional impl's own bounds count as declared inside its
    // body (`impl Encodable for List<T: Encodable>` lets the body
    // call `item.to_wire()`).
    if let Some(overlay) = resolver.bound_overlay
        && overlay.owner == owner
        && let Some(granted) = overlay.bounds.get(index.as_u32() as usize)
    {
        for bound in granted {
            if !declared_bounds.contains(bound) {
                declared_bounds.push(bound.clone());
            }
        }
    }
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
            .map(|(bound, _)| {
                resolver
                    .registry
                    .get(bound.protocol_id)
                    .map(|e| e.identifier.last().to_string())
                    .unwrap_or_else(|| format!("<id {}>", bound.protocol_id))
            })
            .collect();
        diagnostics.push(Diagnostic::error(
            format!(
                "ambiguous method `{method}` on type parameter `{param_name}`: \
                 provided by both `{}` and `{}` in bounds",
                labels[0], labels[1],
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    let (bound, protocol_method) = providers.into_iter().next().expect("len == 1");
    if protocol_method.dispatch != Dispatch::Instance {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot call static method `{method}` of bound protocol on a value of \
                 type parameter `{param_name}`. Use the protocol name to dispatch",
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    let _ = receiver;
    let receiver_type = type_param_ref(owner, index);
    let protocol_subst = protocol_substitution(&bound, receiver_type);
    validate_bounded_args(
        BoundedArgsSite {
            method,
            param_name: &param_name,
            args,
            protocol_method: &protocol_method,
            call_span,
            self_subst: &protocol_subst,
        },
        resolver,
        diagnostics,
    );
    // Substitute Self in the return type with the receiver's
    // type-param (e.g. `Equality.equals? -> Bool` is a no-op, but
    // `Container.first -> Self` would substitute to `T`).
    // Generic protocols (slice 2.7+) will additionally substitute
    // user-declared params against the receiver's type-args.
    substitute(&protocol_method.return_type, &protocol_subst)
}

/// Build the `ResolvedType` for the bare type-parameter `T` at
/// `(owner, index)`, the receiver type a bounded-method call
/// dispatches on. Used to fill the protocol's implicit `Self` slot.
fn type_param_ref(owner: GlobalRegistryId, index: TypeParamIndex) -> ResolvedType {
    ResolvedType::Named {
        resolution: Resolution::TypeParam { owner, index },
        type_args: Vec::new(),
    }
}

/// Single-scope `Self`-substitution for `protocol_id`: slot 0 binds
/// to `receiver_type`. Protocols register their implicit `Self`
/// type-param at index 0 (see
/// `lift_signatures/protocols.rs`), so this is the only slot the
/// substitution needs to fill for non-generic protocols.
fn protocol_substitution(
    bound: &ResolvedProtocolBound,
    receiver_type: ResolvedType,
) -> Substitution {
    let mut args = Vec::with_capacity(bound.args.len() + 1);
    args.push(receiver_type);
    args.extend(bound.args.iter().cloned());
    Substitution::from_args(bound.protocol_id, &args)
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
    declared: &[ResolvedProtocolBound],
    registry: &GlobalRegistry,
) -> Vec<ResolvedProtocolBound> {
    let mut bounds = declared.to_vec();
    for protocol_id in registry.universal_protocol_ids() {
        let bound = ResolvedProtocolBound {
            args: Vec::new(),
            protocol_id,
        };
        if !bounds.contains(&bound) {
            bounds.push(bound);
        }
    }
    bounds
}

/// Walk a type-param's bound list and collect every protocol that
/// declares a method named `method`. Returns clones so the caller
/// can drop the registry borrow before validating arg shapes.
fn collect_bound_providers(
    bounds: &[ResolvedProtocolBound],
    method: &str,
    registry: &GlobalRegistry,
) -> Vec<(ResolvedProtocolBound, ResolvedProtocolMethod)> {
    let mut providers = Vec::new();
    for bound in bounds {
        let Some(entry) = registry.get(bound.protocol_id) else {
            continue;
        };
        let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
            continue;
        };
        if let Some(found) = definition.methods.iter().find(|m| m.name == method) {
            providers.push((bound.clone(), found.clone()));
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
    /// `Self -> <receiver>` substitution applied to each expected
    /// param type before the compatibility check, so a method
    /// declaring `other: Self` accepts an arg whose actual type is
    /// the receiver (rather than the literal `Self` placeholder).
    pub(super) self_subst: &'a Substitution,
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
        self_subst,
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
        let expected_ty = substitute(&expected.ty, self_subst);
        match check_compatible_stamping(&mut arg.value, &actual, &expected_ty, resolver.registry) {
            None => {}
            Some(Mismatch::OutOfRange {
                rendered_value,
                width,
            }) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{method}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        expected.name,
                        display_resolution(&expected_ty, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    arg.span,
                ));
            }
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{method}` expects `{}`, got `{}`",
                        expected.name,
                        display_resolution(&expected_ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}
