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

use koja_ast::ast::{Arg, Diagnostic, Expr};
use koja_ast::coercion::{Coercion, LiteralCoercion};
use koja_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex};
use koja_ast::span::Span;

use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{Dispatch, GlobalKind, GlobalRegistry, ResolvedProtocolMethod};

use koja_ast::ast::PassMode;

use super::super::coercion::{
    Compatible, check_compatible, coercion_annotation_mut, coercion_target_mut,
};
use super::super::ctx::Resolver;
use super::super::moves::move_source_local;
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
    let (protocol_id, protocol_method) = providers.into_iter().next().expect("len == 1");
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
    let _ = receiver;
    let receiver_type = type_param_ref(owner, index);
    let self_subst = self_substitution(protocol_id, receiver_type);
    validate_bounded_args(
        BoundedArgsSite {
            method,
            param_name: &param_name,
            args,
            protocol_method: &protocol_method,
            call_span,
            self_subst: &self_subst,
        },
        resolver,
        diagnostics,
    );
    // Substitute Self in the return type with the receiver's
    // type-param (e.g. `Equality.eq -> Bool` is a no-op, but
    // `Container.first -> Self` would substitute to `T`).
    // Generic protocols (slice 2.7+) will additionally substitute
    // user-declared params against the receiver's type-args.
    substitute(&protocol_method.return_type, &self_subst)
}

/// Build the `ResolvedType` for the bare type-parameter `T` at
/// `(owner, index)` — the receiver type a bounded-method call
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
fn self_substitution(protocol_id: GlobalRegistryId, receiver_type: ResolvedType) -> Substitution {
    Substitution::from_args(protocol_id, &[receiver_type])
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
    /// `Self → <receiver>` substitution applied to each expected
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
        if expected.mode == PassMode::Move
            && let Some(source) = move_source_local(&arg.value, resolver)
        {
            resolver.moves.mark_moved(source, arg.value.span);
        }
        let actual = arg.value.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        let expected_ty = substitute(&expected.ty, self_subst);
        match check_compatible(&arg.value, &actual, &expected_ty, resolver.registry) {
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
                        display_resolution(&expected_ty, resolver.registry),
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
                        display_resolution(&expected_ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}
