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
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;

use bounded::{BoundedCall, resolve_bounded_method_call};
use methods::{
    MethodInferenceTarget, MethodReceiver, classify_receiver, dispatch_mismatch_message,
    function_signature, infer_method_call_type_args, method_lookup_message,
};

use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::types::{display_resolution, verify_bounds};
use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{FunctionSignature, GlobalKind, GlobalRegistry, ResolvedParam};

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

    if receiver_type_params.is_empty() && method_type_params.is_empty() {
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
    // resolution (`Global(struct_id)` with empty `type_args`).
    // Instance dispatch: receiver carries the value's full
    // resolved type. Either way, the same field seeds receiver
    // substitution.
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
        resolver.registry,
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
