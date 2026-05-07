//! Bare-call (`f(args)`) and method-call (`recv.m(args)`) resolution.
//! Both stamp the callee's `GlobalRegistryId` on the AST and validate
//! arity + per-position types. Method calls classify the receiver into
//! a [`MethodReceiver`] (`Static` for `Type.m(...)`, `Instance` for
//! `value.m(...)`) and slice the dispatch / params accordingly.

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{
    GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
    ResolvedProtocolMethod,
};

use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::structs::lookup_type;
use super::types::{display_resolution, verify_bounds};

/// Inputs to [`infer_method_call_type_args`] — bundles the two
/// [`Callee`]s in play (the method and its enclosing type), the
/// receiver's full resolved type (instance dispatch carries the
/// real value; static dispatch supplies an `Unresolved` placeholder
/// that the inference branch ignores), and the explicit param slice
/// (sig.params minus `self` for instance dispatch). The
/// substituted-param return still walks the full `sig.params`.
///
/// `impl_owner` is the trait-impl's owning entry when the method
/// came from `impl P for T` and the impl introduced free type-params.
/// Slice 2.8 anchored those `T`s at the impl entry rather than the
/// receiver struct, so any `TypeParam(impl_id, *)` in the method
/// signature has to substitute through a *separate* scope that
/// unifies the impl's resolved target against the receiver's
/// `ResolvedType`. `None` covers inline struct/enum methods and
/// inherent-impl methods, both of which anchor at the receiver
/// struct id and substitute through `receiver_subst` alone.
struct MethodInferenceTarget<'a> {
    receiver: Callee<'a>,
    method: Callee<'a>,
    receiver_type: &'a ResolvedType,
    explicit_params: &'a [ResolvedParam],
    impl_owner: Option<ImplOwner<'a>>,
}

/// Trait-impl substitution scope passed to [`infer_method_call_type_args`].
/// `target` is the impl's resolved head (e.g. `Bag<TypeParam(impl_id, 0)>`
/// for `impl Pick<T> for Bag<T>`); the inference unifies it against
/// the receiver's `ResolvedType` to bind `subst` slot-by-slot.
struct ImplOwner<'a> {
    callee: Callee<'a>,
    target: &'a ResolvedType,
}

/// Receiver classification for method-call dispatch. `Static` and
/// `Instance` capture the receiver's struct id; `Bounded` captures
/// the type-param's `(owner, index)` for bounded dispatch — the
/// concrete struct id only emerges post-monomorphization.
#[derive(Clone, Copy)]
enum MethodReceiver {
    Static {
        struct_id: GlobalRegistryId,
    },
    Instance {
        struct_id: GlobalRegistryId,
    },
    Bounded {
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    },
}

impl MethodReceiver {
    fn expected_dispatch(self) -> Dispatch {
        match self {
            Self::Static { .. } => Dispatch::Static,
            Self::Instance { .. } | Self::Bounded { .. } => Dispatch::Instance,
        }
    }

    /// Params the user wrote against. Instance / bounded dispatch
    /// absorbs `params[0]` (`self`) into the receiver.
    fn explicit_params(self, params: &[ResolvedParam]) -> &[ResolvedParam] {
        match self {
            Self::Static { .. } => params,
            Self::Instance { .. } | Self::Bounded { .. } => params.get(1..).unwrap_or(&[]),
        }
    }
}

pub(super) fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, resolver, diagnostics);

    let ExprKind::Ident {
        name,
        resolution: ident_resolution,
    } = &mut callee.kind
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports bare-identifier callees (got `{}`)",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let candidate = Identifier::new(resolver.package, vec![name.clone()]);
    let Some((id, entry)) = resolver.registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!("unknown function `{name}`"),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig.clone(),
        GlobalKind::Function(None) => panic!(
            "resolve_call: function `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot call `{name}`: it is a {}, not a function",
                    other.label(),
                ),
                callee.span,
            ));
            return ResolvedType::unresolved();
        }
    };
    let callee_label = entry.identifier.to_string();
    let callee_identifier = entry.identifier.clone();
    let callee_type_params = entry.type_params.clone();

    *ident_resolution = Resolution::Global(id);

    if callee_type_params.is_empty() {
        validate_arg_signature(
            args,
            &sig.params,
            &callee_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        sig.return_type.clone()
    } else {
        let callee = Callee {
            id,
            label: &callee_label,
            type_params: &callee_type_params,
        };
        let (substituted_params, substituted_return) = infer_call_type_args(
            callee,
            &sig,
            args,
            type_args,
            call_span,
            resolver.registry,
            diagnostics,
        );
        validate_arg_signature(
            args,
            &substituted_params,
            &callee_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        substituted_return
    }
}

/// Drive call-site type inference for a generic callee. Unifies each
/// declared param against its arg; surfaces conflicts and phantom
/// params; populates `type_args` on the AST and returns the
/// substituted param list + return type so [`validate_arg_signature`]
/// shows concrete types in arity / type diagnostics rather than
/// leaked `T`.
fn infer_call_type_args(
    callee: Callee<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let mut subst: Vec<Option<ResolvedType>> = vec![None; callee.type_params.len()];
    for (param, arg) in sig.params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        if let Err(conflict) =
            unify_resolved_type(&param.ty, &arg.value.resolution, callee.id, &mut subst)
        {
            emit_conflict(&callee, conflict, arg.span, registry, diagnostics);
        }
    }
    diagnose_phantom_params(&callee, &subst, call_span, diagnostics);
    verify_bounds(callee, &subst, call_span, registry, diagnostics);
    let substituted_params = sig
        .params
        .iter()
        .map(|p| ResolvedParam {
            name: p.name.clone(),
            ty: substitute_resolved_type(&p.ty, &subst, callee.id),
        })
        .collect();
    let substituted_return = substitute_resolved_type(&sig.return_type, &subst, callee.id);
    *out_type_args = subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    (substituted_params, substituted_return)
}

fn emit_conflict(
    callee: &Callee<'_>,
    conflict: Conflict,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "type parameter `{}` of `{}` cannot be both `{}` and `{}`",
            callee.type_params[conflict.param_index],
            callee.label,
            display_resolution(&conflict.prev, registry),
            display_resolution(&conflict.actual, registry),
        ),
        span,
    ));
}

/// Surface a "cannot infer" diagnostic for every type-param slot
/// that stayed `None` after the unification walk. Shared by the
/// bare-call and method-call inference paths.
fn diagnose_phantom_params(
    callee: &Callee<'_>,
    subst: &[Option<ResolvedType>],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, slot) in subst.iter().enumerate() {
        if slot.is_none() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck cannot infer type parameter `{}` of `{}` \
                     from the supplied arguments",
                    callee.type_params[index], callee.label,
                ),
                span,
            ));
        }
    }
}

/// Resolve a method-style call: classify the receiver, look up
/// `<Type>.<method>`, check dispatch matches, then validate args.
/// `out_type_args` is populated when the method or its enclosing
/// type is generic so IR lower can spawn the right monomorphization.
pub(super) fn resolve_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, resolver, diagnostics);

    let Some(method_receiver) = classify_receiver(receiver, resolver, diagnostics) else {
        return ResolvedType::unresolved();
    };

    if let MethodReceiver::Bounded { owner, index } = method_receiver {
        let site = BoundedCall {
            receiver,
            owner,
            index,
            method,
            args,
            call_span,
        };
        return resolve_bounded_method_call(site, resolver, diagnostics);
    }

    let struct_id = match method_receiver {
        MethodReceiver::Static { struct_id } | MethodReceiver::Instance { struct_id } => struct_id,
        MethodReceiver::Bounded { .. } => unreachable!("handled above"),
    };
    let Some(struct_entry) = resolver.registry.get(struct_id) else {
        return ResolvedType::unresolved();
    };
    let receiver_label = struct_entry.identifier.to_string();
    let receiver_type_params = struct_entry.type_params.clone();

    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let Some((method_id, method_entry)) = resolver.registry.lookup(&method_identifier) else {
        diagnostics.push(Diagnostic::error(
            method_lookup_message(method_receiver, struct_entry, method),
            call_span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match function_signature(method_entry) {
        Ok(sig) => sig.clone(),
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return ResolvedType::unresolved();
        }
    };

    let expected = method_receiver.expected_dispatch();
    if sig.dispatch != expected {
        diagnostics.push(Diagnostic::error(
            dispatch_mismatch_message(method_receiver, struct_entry, method_entry, method),
            call_span,
        ));
        return sig.return_type.clone();
    }
    let method_label = method_entry.identifier.to_string();
    let method_identifier = method_entry.identifier.clone();
    let method_type_params = method_entry.type_params.clone();

    // Discover whether `method_id` came from a trait impl with
    // free type-params. Inline / inherent-impl methods skip this
    // path; the `Some` arm captures the impl's resolved target so
    // the inference step can unify it against the receiver and
    // substitute any `TypeParam(impl_id, *)` leaves out of the
    // method's signature.
    let impl_owner_data = trait_impl_owner(method_id, resolver.registry);

    if receiver_type_params.is_empty() && method_type_params.is_empty() && impl_owner_data.is_none()
    {
        validate_arg_signature(
            args,
            method_receiver.explicit_params(&sig.params),
            &method_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        return sig.return_type.clone();
    }

    // Static dispatch: `receiver.resolution` is the type-name's
    // resolution (`Global(struct_id)` with empty `type_args`), so
    // the impl-owner branch's unify naturally short-circuits on
    // empty args. Instance dispatch: receiver carries the value's
    // full resolved type. Either way, the same field flows through.
    let receiver_type = &receiver.resolution;
    let impl_owner = impl_owner_data
        .as_ref()
        .map(|(id, label, type_params, target)| ImplOwner {
            callee: Callee {
                id: *id,
                label,
                type_params,
            },
            target,
        });
    let target = MethodInferenceTarget {
        receiver: Callee {
            id: struct_id,
            label: &receiver_label,
            type_params: &receiver_type_params,
        },
        method: Callee {
            id: method_id,
            label: &method_label,
            type_params: &method_type_params,
        },
        receiver_type,
        explicit_params: method_receiver.explicit_params(&sig.params),
        impl_owner,
    };
    let (substituted_params, substituted_return) = infer_method_call_type_args(
        target,
        &sig,
        args,
        out_type_args,
        call_span,
        resolver.registry,
        diagnostics,
    );
    // "Extend"-style domain check: a method registered at
    // `[receiver_head, method]` only applies to receivers whose
    // full `ResolvedType` matches the method's substituted `self`
    // type. Trait impls on concrete instantiations (e.g.
    // `impl Show for Bag<Int>`) lift `self` as `Bag<Int>`, so calls
    // on `Bag<String>` resolve the lookup but fail this check —
    // matching the design where every impl block adds methods to a
    // specific domain rather than overriding a more-general one.
    if matches!(method_receiver, MethodReceiver::Instance { .. })
        && let Some(self_param) = substituted_params.first()
        && receiver.resolution.is_resolved()
        && self_param.ty.is_resolved()
        && self_param.ty != receiver.resolution
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "no method `{method}` on `{}` (method `{method_label}` is defined for `{}`)",
                display_resolution(&receiver.resolution, resolver.registry),
                display_resolution(&self_param.ty, resolver.registry),
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    validate_arg_signature(
        args,
        method_receiver.explicit_params(&substituted_params),
        &method_identifier,
        call_span,
        resolver.registry,
        diagnostics,
    );
    substituted_return
}

/// Resolve `t.method()` where `t`'s static type is a generic
/// type-parameter `T` whose bounds list provides the method. Walks
/// the bound's protocols, finds the unique provider (or emits
/// not-found / ambiguity), validates args against the protocol
/// method's signature with `Self → t`, and returns the substituted
/// return type. The receiver's `Resolution::TypeParam` stays put;
/// IR-side substitution rewrites it into a concrete type post-mono
/// and the regular `[concrete_target, method_name]` lookup picks up
/// the impl method.
/// Inputs to [`resolve_bounded_method_call`]. Bundles every
/// `recv.method(args)` shard so the helper stays under
/// `too_many_arguments` and reads as a structured site rather than
/// a positional argument soup.
struct BoundedCall<'a> {
    receiver: &'a Expr,
    owner: GlobalRegistryId,
    index: TypeParamIndex,
    method: &'a str,
    args: &'a [Arg],
    call_span: Span,
}

fn resolve_bounded_method_call(
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
    let bounds = resolver
        .registry
        .type_param_bounds(owner)
        .and_then(|all| all.get(index.as_u32() as usize))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let param_name = resolver
        .registry
        .type_param_name(owner, index)
        .unwrap_or("?")
        .to_string();
    if bounds.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!("no method `{method}` on type parameter `{param_name}` (no bounds declared)",),
            call_span,
        ));
        return ResolvedType::unresolved();
    }
    let providers = collect_bound_providers(bounds, method, resolver.registry);
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
        method,
        &param_name,
        args,
        &protocol_method,
        call_span,
        resolver.registry,
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

/// Check arity + per-position type compatibility for a bounded
/// method call. Mirrors [`validate_arg_signature`]'s wording so a
/// "wrong arg type" diagnostic reads identically whether the call
/// dispatches against a struct method or a protocol method.
fn validate_bounded_args(
    method: &str,
    param_name: &str,
    args: &[Arg],
    protocol_method: &ResolvedProtocolMethod,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
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
    for (arg, expected) in args.iter().zip(protocol_method.non_self_params.iter()) {
        let actual = &arg.value.resolution;
        if !actual.is_resolved() {
            continue;
        }
        if actual != &expected.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "argument `{}` to `{method}` expects `{}`, got `{}`",
                    expected.name,
                    display_resolution(&expected.ty, registry),
                    display_resolution(actual, registry),
                ),
                arg.span,
            ));
        }
    }
}

/// Method-call inference. Splits the substitution into two owners:
/// the method's own type-param scope and the receiver's. The receiver
/// scope is seeded by the receiver value's resolved `type_args` (for
/// instance dispatch); the method scope is populated from the
/// arg/param walk just like [`infer_call_type_args`]. `out_type_args`
/// receives the method-scope substitution (the receiver scope is
/// already on the receiver's [`ResolvedType`] and surfaces through
/// the IR's existing struct/enum mangling).
fn infer_method_call_type_args(
    target: MethodInferenceTarget<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let MethodInferenceTarget {
        receiver,
        method,
        receiver_type,
        explicit_params,
        impl_owner,
    } = target;

    let mut receiver_subst: Vec<Option<ResolvedType>> = vec![None; receiver.type_params.len()];
    for (slot, arg) in receiver_subst
        .iter_mut()
        .zip(receiver_type.type_args.iter())
    {
        if arg.resolution.is_resolved() {
            *slot = Some(arg.clone());
        }
    }
    // Trait-impl scope: bind the impl's free type-params by
    // unifying the impl's resolved target against the receiver's
    // full `ResolvedType`. For `impl Pick<T> for Bag<T>` the target
    // is `Bag<TypeParam(impl_id, 0)>`; unifying it against
    // `Bag<Int>` binds `subst[0] = Int` so any
    // `TypeParam(impl_id, 0)` in the method's params/return
    // substitutes through. Static dispatch never reaches here (the
    // bare-call path runs first), and inline / inherent methods
    // skip with `impl_owner: None`.
    let mut impl_subst: Vec<Option<ResolvedType>> = impl_owner
        .as_ref()
        .map(|owner| vec![None; owner.callee.type_params.len()])
        .unwrap_or_default();
    if let Some(owner) = impl_owner.as_ref()
        && !owner.callee.type_params.is_empty()
        && receiver_type.resolution.is_resolved()
        && let Err(conflict) = unify_resolved_type(
            owner.target,
            receiver_type,
            owner.callee.id,
            &mut impl_subst,
        )
    {
        emit_conflict(&owner.callee, conflict, call_span, registry, diagnostics);
    }
    let mut method_subst: Vec<Option<ResolvedType>> = vec![None; method.type_params.len()];
    for (param, arg) in explicit_params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        if !method.type_params.is_empty()
            && let Err(conflict) = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                method.id,
                &mut method_subst,
            )
        {
            emit_conflict(&method, conflict, arg.span, registry, diagnostics);
        }
        if !receiver.type_params.is_empty()
            && let Err(conflict) = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                receiver.id,
                &mut receiver_subst,
            )
        {
            emit_conflict(&receiver, conflict, arg.span, registry, diagnostics);
        }
    }
    diagnose_phantom_params(&method, &method_subst, call_span, diagnostics);
    diagnose_phantom_params(&receiver, &receiver_subst, call_span, diagnostics);
    verify_bounds(method, &method_subst, call_span, registry, diagnostics);
    verify_bounds(receiver, &receiver_subst, call_span, registry, diagnostics);
    if let Some(owner) = impl_owner.as_ref() {
        diagnose_phantom_params(&owner.callee, &impl_subst, call_span, diagnostics);
        verify_bounds(owner.callee, &impl_subst, call_span, registry, diagnostics);
    }
    let substituted_params: Vec<ResolvedParam> = sig
        .params
        .iter()
        .map(|p| {
            let with_method = substitute_resolved_type(&p.ty, &method_subst, method.id);
            let with_impl = match impl_owner.as_ref() {
                Some(owner) => substitute_resolved_type(&with_method, &impl_subst, owner.callee.id),
                None => with_method,
            };
            let with_receiver = substitute_resolved_type(&with_impl, &receiver_subst, receiver.id);
            ResolvedParam {
                name: p.name.clone(),
                ty: with_receiver,
            }
        })
        .collect();
    let with_method_return = substitute_resolved_type(&sig.return_type, &method_subst, method.id);
    let with_impl_return = match impl_owner.as_ref() {
        Some(owner) => substitute_resolved_type(&with_method_return, &impl_subst, owner.callee.id),
        None => with_method_return,
    };
    let substituted_return =
        substitute_resolved_type(&with_impl_return, &receiver_subst, receiver.id);
    *out_type_args = method_subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    (substituted_params, substituted_return)
}

/// Inspect the receiver and pick the dispatch path. Stamps both the
/// inner `Ident` and outer `Expr` resolutions so seal sees a fully
/// populated tree.
fn classify_receiver(
    receiver: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodReceiver> {
    if let Some(receiver_name) = bare_ident_name(&receiver.kind) {
        let receiver_path = [receiver_name.to_string()];
        if let Some((struct_id, struct_entry)) =
            lookup_type(&receiver_path, resolver.package, resolver.registry)
            && matches!(
                struct_entry.kind,
                GlobalKind::Enum(_) | GlobalKind::Struct(_)
            )
        {
            if let ExprKind::Ident {
                resolution: receiver_resolution,
                ..
            } = &mut receiver.kind
            {
                *receiver_resolution = Resolution::Global(struct_id);
            }
            receiver.resolution = ResolvedType::leaf(Resolution::Global(struct_id));
            return Some(MethodReceiver::Static { struct_id });
        }
    }

    resolve_expr(receiver, resolver, diagnostics);
    if !receiver.resolution.is_resolved() {
        // Receiver already triggered its own diagnostic.
        return None;
    }
    match receiver.resolution.resolution {
        Resolution::Global(struct_id) => {
            let entry = resolver.registry.get(struct_id)?;
            if !matches!(entry.kind, GlobalKind::Enum(_) | GlobalKind::Struct(_)) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "instance method receiver must be a struct or enum value (`{}` is a {})",
                        entry.identifier,
                        entry.kind.label(),
                    ),
                    receiver.span,
                ));
                return None;
            }
            Some(MethodReceiver::Instance { struct_id })
        }
        Resolution::TypeParam { owner, index } => Some(MethodReceiver::Bounded { owner, index }),
        _ => {
            diagnostics.push(Diagnostic::error(
                "instance method receiver must have a struct or enum type".to_string(),
                receiver.span,
            ));
            None
        }
    }
}

fn bare_ident_name(kind: &ExprKind) -> Option<&str> {
    match kind {
        ExprKind::Ident { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn function_signature(entry: &RegistryEntry) -> Result<&FunctionSignature, Diagnostic> {
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => Ok(sig),
        GlobalKind::Function(None) => panic!(
            "resolve method call: function `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        other => Err(Diagnostic::error(
            format!(
                "cannot call `{}`: it is a {}, not a function",
                entry.identifier,
                other.label(),
            ),
            entry.span,
        )),
    }
}

fn method_lookup_message(
    receiver: MethodReceiver,
    struct_entry: &RegistryEntry,
    method: &str,
) -> String {
    match receiver {
        MethodReceiver::Static { .. } => format!(
            "`{}` has no static method `{method}`",
            struct_entry.identifier,
        ),
        MethodReceiver::Instance { .. } => {
            format!("`{}` has no method `{method}`", struct_entry.identifier,)
        }
        MethodReceiver::Bounded { .. } => unreachable!("bounded receivers don't reach this path"),
    }
}

fn dispatch_mismatch_message(
    receiver: MethodReceiver,
    struct_entry: &RegistryEntry,
    method_entry: &RegistryEntry,
    method: &str,
) -> String {
    match receiver {
        MethodReceiver::Static { .. } => format!(
            "cannot call instance method `{}` as a static method — call it on a value of `{}` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Instance { .. } => format!(
            "cannot call static method `{}` on a value — call it as `{}.{method}(...)` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Bounded { .. } => unreachable!("bounded receivers don't reach this path"),
    }
}

/// Resolve every call argument. Named args diagnose up front but
/// resolution still proceeds so seal walks a populated tree.
fn resolve_args(args: &mut [Arg], resolver: &mut Resolver<'_>, diagnostics: &mut Vec<Diagnostic>) {
    for arg in args.iter_mut() {
        if let Some(name) = arg.name.as_ref() {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck does not yet support named arguments (got `{name}`)",),
                arg.span,
            ));
        }
        resolve_expr(&mut arg.value, resolver, diagnostics);
    }
}

/// Check arg arity + per-position type compatibility. Diagnostics
/// use the callee's fully-qualified [`Identifier`].
fn validate_arg_signature(
    args: &[Arg],
    expected_params: &[ResolvedParam],
    callee: &Identifier,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if args.len() != expected_params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{callee}` expects {} argument{}, got {}",
                expected_params.len(),
                if expected_params.len() == 1 { "" } else { "s" },
                args.len(),
            ),
            call_span,
        ));
        return;
    }

    for (arg, param) in args.iter().zip(expected_params.iter()) {
        let actual = &arg.value.resolution;
        if !actual.is_resolved() {
            continue;
        }
        if actual != &param.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "argument `{}` to `{callee}` expects `{}`, got `{}`",
                    param.name,
                    display_resolution(&param.ty, registry),
                    display_resolution(actual, registry),
                ),
                arg.span,
            ));
        }
    }
}

/// Discover whether `method_id` came from a `GlobalKind::ProtocolImpl`
/// entry with non-empty type-params (i.e. `impl P for T` where `T`
/// carries free type-params, slice 2.8). Returns the impl's
/// `(id, label, type_params, target)` so the caller can construct
/// an [`ImplOwner`] without re-borrowing the registry. Returns
/// `None` for inline / inherent-impl methods (no impl entry to
/// anchor at) and for trait impls with no free type-params (e.g.
/// `impl P for Foo` on a non-generic target — receiver substitution
/// alone covers it).
fn trait_impl_owner(
    method_id: GlobalRegistryId,
    registry: &GlobalRegistry,
) -> Option<(GlobalRegistryId, String, Vec<String>, ResolvedType)> {
    let impl_id = registry.find_protocol_impl_owning(method_id)?;
    let entry = registry.get(impl_id)?;
    if entry.type_params.is_empty() {
        return None;
    }
    let GlobalKind::ProtocolImpl(Some(definition)) = &entry.kind else {
        return None;
    };
    Some((
        impl_id,
        entry.identifier.to_string(),
        entry.type_params.clone(),
        definition.target.clone(),
    ))
}
