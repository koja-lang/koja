//! Bare-call (`f(args)`) and method-call (`recv.m(args)`) resolution.
//! Both stamp the callee's `GlobalRegistryId` on the AST and validate
//! arity + per-position types.
//!
//! # Module layout
//!
//! - [`methods`] — receiver classification (`Static` / `Instance` /
//!   `Bounded`), dual-scope (receiver + method) type-arg inference,
//!   and the small lookup / diagnostic-shape helpers re-used by
//!   [`resolve_method_call`].
//! - [`bounded`] — `t.m(args)` against a type-param receiver:
//!   protocol-method lookup against the type-param's bounds list,
//!   ambiguity / not-found diagnostics, and arg validation against
//!   the protocol's signature.
//!
//! Both flavors of call entry point ([`resolve_call`] and
//! [`resolve_method_call`]) live in this file alongside the
//! cross-flavor helpers ([`emit_conflict`] /
//! [`diagnose_phantom_params`] / [`resolve_args`] /
//! [`validate_arg_signature`]) so submodules need only sibling
//! `pub(super)` visibility.

mod bounded;
mod methods;

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{
    AnonymousKind, FnParam, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;

use bounded::{BoundedCall, resolve_bounded_method_call};
use methods::{
    MethodInferenceTarget, MethodReceiver, classify_receiver, dispatch_mismatch_message,
    function_signature, infer_method_call_type_args, method_lookup_message,
};

use super::coercion::{Compatible, check_compatible, coercion_span};
use super::ctx::{Callee, Resolver};
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::types::{display_resolution, verify_bounds};
use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
};

pub(super) fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ExprKind::Ident {
        name,
        resolution: ident_resolution,
    } = &mut callee.kind
    else {
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports bare-identifier callees (got `{}`)",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    if let Some((local_id, local_ty)) = resolver.scope.lookup(name) {
        let local_ty = local_ty.clone();
        return resolve_local_call(
            name,
            ident_resolution,
            local_id,
            local_ty,
            &mut callee.resolution,
            args,
            call_span,
            callee.span,
            resolver,
            diagnostics,
        );
    }

    let Some((id, entry)) = lookup_bare_callee(
        name,
        resolver.package,
        resolver.enclosing_type,
        resolver.registry,
    ) else {
        resolve_args(args, None, resolver, diagnostics);
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
            resolve_args(args, None, resolver, diagnostics);
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
        resolve_args(args, Some(&sig.params), resolver, diagnostics);
        validate_arg_signature(
            args,
            &sig.params,
            &callee_identifier,
            call_span,
            resolver,
            diagnostics,
        );
        sig.return_type.clone()
    } else {
        resolve_non_closure_args(args, resolver, diagnostics);
        let callee = Callee {
            id,
            label: &callee_label,
            type_params: &callee_type_params,
        };
        let partial_subst = partial_unify_call(callee, &sig, args, resolver.registry, diagnostics);
        let partially_substituted_params = sig
            .params
            .iter()
            .map(|p| ResolvedParam {
                mode: p.mode,
                name: p.name.clone(),
                ty: substitute_resolved_type(&p.ty, &partial_subst, callee.id),
            })
            .collect::<Vec<_>>();
        resolve_closure_args(args, &partially_substituted_params, resolver, diagnostics);
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
            resolver,
            diagnostics,
        );
        substituted_return
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
    let Some(method_receiver) = classify_receiver(receiver, resolver, diagnostics) else {
        resolve_args(args, None, resolver, diagnostics);
        return ResolvedType::unresolved();
    };

    if let MethodReceiver::Bounded { owner, index } = method_receiver {
        resolve_args(args, None, resolver, diagnostics);
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

    if receiver_type_params.is_empty() && method_type_params.is_empty() {
        resolve_args(
            args,
            Some(method_receiver.explicit_params(&sig.params)),
            resolver,
            diagnostics,
        );
        validate_arg_signature(
            args,
            method_receiver.explicit_params(&sig.params),
            &method_identifier,
            call_span,
            resolver,
            diagnostics,
        );
        return sig.return_type.clone();
    }

    // Static dispatch: `receiver.resolution` is the type-name's
    // resolution (`Global(struct_id)` with empty `type_args`).
    // Instance dispatch: receiver carries the value's full
    // resolved type. Either way, the same field seeds receiver
    // substitution.
    let receiver_callee = Callee {
        id: struct_id,
        label: &receiver_label,
        type_params: &receiver_type_params,
    };
    let method_callee = Callee {
        id: method_id,
        label: &method_label,
        type_params: &method_type_params,
    };
    resolve_non_closure_args(args, resolver, diagnostics);
    let (partial_receiver_subst, partial_method_subst) = partial_unify_method_call(
        receiver_callee,
        method_callee,
        method_receiver.explicit_params(&sig.params),
        &receiver.resolution,
        args,
    );
    let partially_substituted_params = sig
        .params
        .iter()
        .map(|p| {
            let with_method =
                substitute_resolved_type(&p.ty, &partial_method_subst, method_callee.id);
            let with_receiver =
                substitute_resolved_type(&with_method, &partial_receiver_subst, receiver_callee.id);
            ResolvedParam {
                mode: p.mode,
                name: p.name.clone(),
                ty: with_receiver,
            }
        })
        .collect::<Vec<_>>();
    resolve_closure_args(
        args,
        method_receiver.explicit_params(&partially_substituted_params),
        resolver,
        diagnostics,
    );

    let target = MethodInferenceTarget {
        receiver: receiver_callee,
        method: method_callee,
        receiver_type: &receiver.resolution,
        explicit_params: method_receiver.explicit_params(&sig.params),
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
        resolver,
        diagnostics,
    );
    substituted_return
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
            mode: p.mode,
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

/// Surface a "this generic-param slot got two distinct types"
/// diagnostic. Shared between bare-call and method-call inference.
pub(super) fn emit_conflict(
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
pub(super) fn diagnose_phantom_params(
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

/// Resolve a bare call `name(...)`: prioritize the enclosing
/// scope, then fall through to the package scope. Inside a
/// struct/enum method, `Package.Enclosing.name` wins over
/// `Package.name` when both exist; the escape hatch for callers
/// who want the package-level function in the conflict case is
/// to fully qualify (`Global.name()`), which goes through path-
/// call resolution and never reaches this helper. Free functions
/// and file bodies pass `enclosing_type = None` and skip the
/// first step. Takes the registry directly (rather than the full
/// [`Resolver`]) so the caller keeps `&mut` access for
/// diagnostics and arg resolution on the not-found path.
fn lookup_bare_callee<'a>(
    name: &str,
    package: &str,
    enclosing_type: Option<&str>,
    registry: &'a GlobalRegistry,
) -> Option<(GlobalRegistryId, &'a RegistryEntry)> {
    if let Some(enclosing) = enclosing_type {
        let scoped = Identifier::new(package, vec![enclosing.to_string(), name.to_string()]);
        if let Some(found) = registry.lookup(&scoped) {
            return Some(found);
        }
    }
    registry.lookup(&Identifier::new(package, vec![name.to_string()]))
}

/// Resolve every call argument with optional per-position expected
/// types. Named args diagnose up front but resolution still proceeds
/// so seal walks a populated tree. The expected type at each position
/// flows into the corresponding arg via [`resolve_expr_with_expected`]
/// so closure args can pull their param/return shape from the
/// callee's signature when the user omits annotations.
fn resolve_args(
    args: &mut [Arg],
    expected: Option<&[ResolvedParam]>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, arg) in args.iter_mut().enumerate() {
        diagnose_named_arg(arg, diagnostics);
        let expected_ty = expected.and_then(|params| params.get(index)).map(|p| &p.ty);
        resolve_expr_with_expected(&mut arg.value, expected_ty, resolver, diagnostics);
    }
}

/// First-pass arg resolution for generic callees: resolve every arg
/// that isn't a closure expression so type-arg inference has
/// something to unify against. Closure args wait for the second pass
/// once their expected `fn (T) -> U` shape is known.
fn resolve_non_closure_args(
    args: &mut [Arg],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for arg in args.iter_mut() {
        diagnose_named_arg(arg, diagnostics);
        if is_closure_expr(&arg.value.kind) {
            continue;
        }
        resolve_expr(&mut arg.value, resolver, diagnostics);
    }
}

/// Second-pass arg resolution for generic callees: walk closure args
/// with the substituted param type as the expected hint so closure
/// param/return slots inherit any type-args inferred from the
/// non-closure args.
fn resolve_closure_args(
    args: &mut [Arg],
    expected: &[ResolvedParam],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, arg) in args.iter_mut().enumerate() {
        if !is_closure_expr(&arg.value.kind) {
            continue;
        }
        let expected_ty = expected.get(index).map(|p| &p.ty);
        resolve_expr_with_expected(&mut arg.value, expected_ty, resolver, diagnostics);
    }
}

fn diagnose_named_arg(arg: &Arg, diagnostics: &mut Vec<Diagnostic>) {
    if let Some(name) = arg.name.as_ref() {
        diagnostics.push(Diagnostic::error(
            format!("alpha typecheck does not yet support named arguments (got `{name}`)",),
            arg.span,
        ));
    }
}

fn is_closure_expr(kind: &ExprKind) -> bool {
    matches!(
        kind,
        ExprKind::Closure { .. } | ExprKind::ShortClosure { .. }
    )
}

/// Run a single unification pass without diagnosing conflicts —
/// used for partial inference before closure args resolve.
/// Diagnostics for conflicts and phantom params still come from the
/// final [`infer_call_type_args`] run after every arg has resolved.
fn partial_unify_call(
    callee: Callee<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    _registry: &GlobalRegistry,
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<ResolvedType>> {
    let mut subst: Vec<Option<ResolvedType>> = vec![None; callee.type_params.len()];
    for (param, arg) in sig.params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        let _ = unify_resolved_type(&param.ty, &arg.value.resolution, callee.id, &mut subst);
    }
    subst
}

/// Method-call counterpart to [`partial_unify_call`]. Seeds the
/// receiver substitution from the receiver's resolved type-args
/// (mirroring [`infer_method_call_type_args`]) and unifies each
/// non-closure arg against its declared param under both the receiver
/// and method scopes. Returns `(receiver_subst, method_subst)` for
/// caller-driven substitution before closure args resolve.
fn partial_unify_method_call(
    receiver: Callee<'_>,
    method: Callee<'_>,
    explicit_params: &[ResolvedParam],
    receiver_type: &ResolvedType,
    args: &[Arg],
) -> (Vec<Option<ResolvedType>>, Vec<Option<ResolvedType>>) {
    let mut receiver_subst: Vec<Option<ResolvedType>> = vec![None; receiver.type_params.len()];
    let receiver_args: &[ResolvedType] = match receiver_type {
        ResolvedType::Named { type_args, .. } => type_args,
        _ => &[],
    };
    for (slot, arg) in receiver_subst.iter_mut().zip(receiver_args.iter()) {
        if arg.is_resolved() {
            *slot = Some(arg.clone());
        }
    }
    let mut method_subst: Vec<Option<ResolvedType>> = vec![None; method.type_params.len()];
    for (param, arg) in explicit_params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        if !method.type_params.is_empty() {
            let _ = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                method.id,
                &mut method_subst,
            );
        }
        if !receiver.type_params.is_empty() {
            let _ = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                receiver.id,
                &mut receiver_subst,
            );
        }
    }
    (receiver_subst, method_subst)
}

/// Check arg arity + per-position type compatibility. Diagnostics
/// use the callee's fully-qualified [`Identifier`]. Per-position
/// equivalence runs through [`check_compatible`] so a numeric
/// literal flowing into a narrow-int / narrow-float param coerces
/// when its compile-time value fits the param's range; the
/// recorded coercion lands on the resolver's program-wide table
/// for IR lower to consume.
fn validate_arg_signature(
    args: &[Arg],
    expected_params: &[ResolvedParam],
    callee: &Identifier,
    call_span: Span,
    resolver: &mut Resolver<'_>,
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
        match check_compatible(&arg.value, actual, &param.ty, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                resolver.coercions.insert(coercion_span(&arg.value), width);
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    arg.span,
                ));
            }
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee}` expects `{}`, got `{}`",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        display_resolution(actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}

/// Closure-typed local-call resolution: stamps the ident as
/// [`Resolution::Local`], threads the function's params as expected
/// arg types, and validates arity + per-position types. Non-function
/// locals diagnose and return [`ResolvedType::unresolved`].
#[allow(clippy::too_many_arguments)]
fn resolve_local_call(
    name: &str,
    ident_resolution: &mut Resolution,
    local_id: LocalId,
    local_ty: ResolvedType,
    callee_ty_slot: &mut ResolvedType,
    args: &mut [Arg],
    call_span: Span,
    callee_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ResolvedType::Anonymous(AnonymousKind::Function {
        params: fn_params,
        ret,
    }) = &local_ty
    else {
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot call `{name}`: it is `{}`, not a function",
                display_resolution(&local_ty, resolver.registry),
            ),
            callee_span,
        ));
        return ResolvedType::unresolved();
    };
    *ident_resolution = Resolution::Local(local_id);
    *callee_ty_slot = local_ty.clone();
    let expected_params = synthesize_local_call_params(fn_params);
    resolve_args(args, Some(&expected_params), resolver, diagnostics);
    validate_local_call_signature(
        args,
        &expected_params,
        name,
        call_span,
        resolver,
        diagnostics,
    );
    (**ret).clone()
}

/// Build per-position [`ResolvedParam`]s for a local closure call.
/// Names are synthesized as `arg<index>` so arity / type
/// diagnostics still surface a label without depending on a
/// signature decl that doesn't exist.
fn synthesize_local_call_params(fn_params: &[FnParam]) -> Vec<ResolvedParam> {
    fn_params
        .iter()
        .enumerate()
        .map(|(index, p)| ResolvedParam {
            mode: p.mode,
            name: format!("arg{index}"),
            ty: p.ty.clone(),
        })
        .collect()
}

/// Local-call counterpart to [`validate_arg_signature`]. Same
/// invariants (arity match + per-position type match plus the
/// literal-fit coercion fallback) but uses a bare `&str` callee
/// label (the local's surface name) so the diagnostic doesn't
/// fabricate a fully-qualified identifier the user never wrote.
fn validate_local_call_signature(
    args: &[Arg],
    expected_params: &[ResolvedParam],
    callee_label: &str,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if args.len() != expected_params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{callee_label}` expects {} argument{}, got {}",
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
        match check_compatible(&arg.value, actual, &param.ty, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                resolver.coercions.insert(coercion_span(&arg.value), width);
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee_label}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    arg.span,
                ));
            }
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee_label}` expects `{}`, got `{}`",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        display_resolution(actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}
