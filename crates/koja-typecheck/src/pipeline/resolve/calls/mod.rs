//! Bare-call (`f(args)`) and method-call (`recv.m(args)`) resolution.
//! Both stamp the callee's `GlobalRegistryId` on the AST and validate
//! arity + per-position types.
//!
//! # Module layout
//!
//! - [`methods`]: receiver classification (`Static` / `Instance` /
//!   `Bounded`), dual-scope (receiver + method) type-arg inference,
//!   and the small lookup / diagnostic-shape helpers re-used by
//!   [`resolve_method_call`].
//! - [`bounded`]: `t.m(args)` against a type-param receiver,
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

use koja_ast::ast::{Arg, Diagnostic, Expr, ExprKind, Literal};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use koja_ast::labels::expr_kind_label;
use koja_ast::span::Span;

use bounded::{BoundedCall, resolve_bounded_method_call};
use methods::{
    MethodInferenceOutputs, MethodInferenceTarget, MethodReceiver, classify_receiver,
    dispatch_mismatch_message, function_signature, infer_method_call_type_args,
    method_lookup_message, seed_receiver_subst,
};

use crate::pipeline::unify::{Conflict, Substitution, substitute};
use crate::registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam, VisibilityScope,
};

use super::coercion::{Mismatch, check_compatible_stamping};
use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr_with_expected;
use super::inference::{PhantomContext, fill_from_expected, finalize_inference, unify_pairs};
use super::process::check_monitor_call_site;
use super::types::{display_resolution, lookup_type};

/// Co-traveling call-site context shared by [`resolve_call`] /
/// [`resolve_method_call`] and the inner generic-inference helpers.
/// Bundles the three slots that always thread together: the AST's
/// `type_args` output vec, the surrounding expected return type
/// (when bidirectional inference applies), and the call's span (for
/// diagnostics). Inputs (receiver / method / args / sig) and the
/// resolver/diagnostics env stay as separate params, these three
/// just happen to always move as a group.
pub(super) struct CallSite<'a> {
    pub(super) out_type_args: &'a mut Vec<ResolvedType>,
    pub(super) expected: Option<&'a ResolvedType>,
    pub(super) span: Span,
}

pub(super) fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    site: CallSite<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let call_span = site.span;
    let ExprKind::Ident {
        name,
        resolution: ident_resolution,
    } = &mut callee.kind
    else {
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck only supports bare-identifier callees (got `{}`)",
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
    check_callee_visibility(entry, resolver, call_span, diagnostics);

    let sig = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig.clone(),
        GlobalKind::Function(None) => panic!(
            "resolve_call: function `{}` has no lifted signature: \
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
    let function = FunctionCallee {
        id,
        identifier: entry.identifier.clone(),
        signature: sig,
        type_params: entry.type_params.clone(),
    };

    *ident_resolution = Resolution::Global(id);
    resolve_function_call(function, args, site, resolver, diagnostics)
}

/// A function callee whose registry entry has already been located
/// and kind-checked. Owned clones so the registry borrow ends before
/// the `&mut Resolver` work in [`resolve_function_call`].
struct FunctionCallee {
    id: GlobalRegistryId,
    identifier: Identifier,
    signature: FunctionSignature,
    type_params: Vec<String>,
}

/// Shared tail for bare (`f(args)`) and package-qualified
/// (`Pkg.f(args)`) function calls: argument resolution, arity and
/// per-position type validation, and type-arg inference for generic
/// callees. Returns the (substituted) return type.
fn resolve_function_call(
    function: FunctionCallee,
    args: &mut [Arg],
    site: CallSite<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let call_span = site.span;
    let FunctionCallee {
        id,
        identifier,
        signature: sig,
        type_params,
    } = function;
    let label = identifier.to_string();

    if type_params.is_empty() {
        resolve_args(args, Some(&sig.params), resolver, diagnostics);
        validate_arg_signature(
            args,
            &sig.params,
            &identifier,
            call_span,
            resolver,
            diagnostics,
        );
        sig.return_type.clone()
    } else {
        let callee = Callee {
            id,
            label: &label,
            type_params: &type_params,
        };
        let mut hint_subst = Substitution::single(callee.id, callee.type_params.len());
        if let Some(hint) = site.expected {
            fill_from_expected(&sig.return_type, hint, &mut hint_subst, resolver.registry);
        }
        let hinted_params = substitute_params(&sig.params, &hint_subst);
        resolve_non_closure_args(args, Some(&hinted_params), resolver, diagnostics);
        let mut partial_subst = Substitution::single(callee.id, callee.type_params.len());
        let partial_pairs = sig
            .params
            .iter()
            .zip(args.iter())
            .map(|(p, a)| (&p.ty, &a.value.resolution, ()));
        unify_pairs(
            partial_pairs,
            &mut partial_subst,
            resolver.registry,
            |_, _| {},
        );
        let partially_substituted_params = substitute_params(&sig.params, &partial_subst);
        resolve_closure_args(args, &partially_substituted_params, resolver, diagnostics);
        let (substituted_params, substituted_return) =
            infer_call_type_args(callee, &sig, args, site, resolver.registry, diagnostics);
        validate_arg_signature(
            args,
            &substituted_params,
            &identifier,
            call_span,
            resolver,
            diagnostics,
        );
        substituted_return
    }
}

/// Either a normal method dispatch (return type only), a
/// field-as-callable fallback whose substituted fn-type +
/// return type the caller uses to rewrite the AST, or a
/// package-qualified function call (`Pkg.f(args)`) the caller
/// rewrites to a plain `Call`.
pub(super) enum MethodCallOutcome {
    FieldCall {
        callee_ty: ResolvedType,
        return_ty: ResolvedType,
    },
    Method(ResolvedType),
    PackageCall {
        fn_id: GlobalRegistryId,
        return_ty: ResolvedType,
    },
}

/// `resolve_method_call` wrapper that performs the AST rewrite when
/// the field-as-callable fallback fires. Lifted out of the main
/// `resolve_expr` match so the rewrite has `&mut Expr` access.
pub(super) fn resolve_method_call_expr(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let span = expr.span;
    let outcome = match &mut expr.kind {
        ExprKind::MethodCall {
            args,
            method,
            receiver,
            type_args,
        } => resolve_method_call(
            receiver,
            method,
            args,
            CallSite {
                expected,
                out_type_args: type_args,
                span,
            },
            resolver,
            diagnostics,
        ),
        other => unreachable!(
            "resolve_method_call_expr called with non-MethodCall ExprKind: {}",
            expr_kind_label(other),
        ),
    };
    match outcome {
        MethodCallOutcome::FieldCall {
            callee_ty,
            return_ty,
        } => {
            rewrite_method_call_to_field_call(expr, callee_ty);
            return_ty
        }
        MethodCallOutcome::Method(ty) => ty,
        MethodCallOutcome::PackageCall { fn_id, return_ty } => {
            rewrite_method_call_to_package_call(expr, fn_id);
            return_ty
        }
    }
}

/// Stamp `expr.kind` as `Call { callee: Ident("Pkg.f", Global(fn_id)), args }`
/// in place, preserving any inferred `type_args`, so IR lower's
/// existing bare-call path handles a package-qualified call without
/// further branching. The receiver `Ident` is discarded, it named a
/// package, not a value.
fn rewrite_method_call_to_package_call(expr: &mut Expr, fn_id: GlobalRegistryId) {
    let span = expr.span;
    let stub = ExprKind::Literal {
        value: Literal::Unit,
    };
    let ExprKind::MethodCall {
        args,
        method,
        receiver,
        type_args,
    } = std::mem::replace(&mut expr.kind, stub)
    else {
        unreachable!("rewrite_method_call_to_package_call called on non-MethodCall ExprKind");
    };
    let package = match &receiver.kind {
        ExprKind::Ident { name, .. } => name.clone(),
        _ => unreachable!("package call receiver is always a bare Ident"),
    };
    let callee = Expr::new(
        ExprKind::Ident {
            name: format!("{package}.{method}"),
            resolution: Resolution::Global(fn_id),
        },
        span,
    );
    expr.kind = ExprKind::Call {
        args,
        callee: Box::new(callee),
        type_args,
    };
}

/// Stamp `expr.kind` as `Call { callee: FieldAccess(recv, method), args }`
/// in place. `callee_ty` is the substituted fn-type the
/// field-as-callable branch resolved against.
fn rewrite_method_call_to_field_call(expr: &mut Expr, callee_ty: ResolvedType) {
    let span = expr.span;
    let stub = ExprKind::Literal {
        value: Literal::Unit,
    };
    let ExprKind::MethodCall {
        args,
        method,
        receiver,
        ..
    } = std::mem::replace(&mut expr.kind, stub)
    else {
        unreachable!("rewrite_method_call_to_field_call called on non-MethodCall ExprKind");
    };
    let mut callee = Expr::new(
        ExprKind::FieldAccess {
            field: method,
            receiver,
        },
        span,
    );
    callee.resolution = callee_ty;
    expr.kind = ExprKind::Call {
        args,
        callee: Box::new(callee),
        type_args: Vec::new(),
    };
}

/// Resolve a method-style call: classify the receiver, look up
/// `<Type>.<method>`, check dispatch matches, validate args. On
/// instance dispatch with no matching method, falls through to
/// [`try_field_callable`] for the `recv.field(args)` shape.
pub(super) fn resolve_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    site: CallSite<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> MethodCallOutcome {
    let CallSite {
        out_type_args,
        expected,
        span: call_span,
    } = site;

    if let Some(outcome) = try_package_function_call(
        receiver,
        method,
        args,
        CallSite {
            out_type_args: &mut *out_type_args,
            expected,
            span: call_span,
        },
        resolver,
        diagnostics,
    ) {
        return outcome;
    }

    let Some(method_receiver) = classify_receiver(receiver, resolver, diagnostics) else {
        resolve_args(args, None, resolver, diagnostics);
        return MethodCallOutcome::Method(ResolvedType::unresolved());
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
        return MethodCallOutcome::Method(resolve_bounded_method_call(site, resolver, diagnostics));
    }

    let struct_id = match method_receiver {
        MethodReceiver::Static { struct_id } | MethodReceiver::Instance { struct_id } => struct_id,
        MethodReceiver::Bounded { .. } => unreachable!("handled above"),
    };
    let Some(struct_entry) = resolver.registry.get(struct_id) else {
        return MethodCallOutcome::Method(ResolvedType::unresolved());
    };
    let receiver_label = struct_entry.identifier.to_string();
    // A protocol's type params (`Self`, `C`, ...) belong to its
    // contract, not to its extend-registered statics. Blanking them
    // keeps inference from demanding args no static can constrain.
    let receiver_type_params = if matches!(struct_entry.kind, GlobalKind::Protocol(_)) {
        Vec::new()
    } else {
        struct_entry.type_params.clone()
    };

    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let Some((method_id, method_entry)) = resolver.registry.lookup(&method_identifier) else {
        if matches!(method_receiver, MethodReceiver::Instance { .. })
            && let Some(field_call) =
                try_field_callable(struct_id, receiver, method, args, resolver, diagnostics)
        {
            return field_call;
        }
        diagnostics.push(Diagnostic::error(
            method_lookup_message(method_receiver, struct_entry, method),
            call_span,
        ));
        return MethodCallOutcome::Method(ResolvedType::unresolved());
    };
    check_callee_visibility(method_entry, resolver, call_span, diagnostics);

    let sig = match function_signature(method_entry) {
        Ok(sig) => sig.clone(),
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return MethodCallOutcome::Method(ResolvedType::unresolved());
        }
    };

    let expected_dispatch = method_receiver.expected_dispatch();
    if sig.dispatch != expected_dispatch {
        diagnostics.push(Diagnostic::error(
            dispatch_mismatch_message(method_receiver, struct_entry, method_entry, method),
            call_span,
        ));
        return MethodCallOutcome::Method(sig.return_type.clone());
    }
    let method_label = method_entry.identifier.to_string();
    let method_identifier = method_entry.identifier.clone();
    let method_type_params = method_entry.type_params.clone();
    check_monitor_call_site(&method_identifier, call_span, resolver, diagnostics);

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
        return MethodCallOutcome::Method(sig.return_type.clone());
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
    let mut hint_subst = Substitution::dual(
        receiver_callee.id,
        receiver_callee.type_params.len(),
        method_callee.id,
        method_callee.type_params.len(),
    );
    seed_receiver_subst(
        &mut hint_subst,
        receiver_callee.id,
        &receiver.resolution,
        resolver.registry,
    );
    if let Some(hint) = expected {
        fill_from_expected(&sig.return_type, hint, &mut hint_subst, resolver.registry);
    }
    let hinted_params = substitute_params(&sig.params, &hint_subst);
    resolve_non_closure_args(
        args,
        Some(method_receiver.explicit_params(&hinted_params)),
        resolver,
        diagnostics,
    );
    let mut partial_subst = Substitution::dual(
        receiver_callee.id,
        receiver_callee.type_params.len(),
        method_callee.id,
        method_callee.type_params.len(),
    );
    seed_receiver_subst(
        &mut partial_subst,
        receiver_callee.id,
        &receiver.resolution,
        resolver.registry,
    );
    let explicit = method_receiver.explicit_params(&sig.params);
    let partial_pairs = explicit
        .iter()
        .zip(args.iter())
        .map(|(p, a)| (&p.ty, &a.value.resolution, ()));
    unify_pairs(
        partial_pairs,
        &mut partial_subst,
        resolver.registry,
        |_, _| {},
    );
    let partially_substituted_params = substitute_params(&sig.params, &partial_subst);
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
        expected,
    };
    let mut receiver_args_inferred: Vec<ResolvedType> = Vec::new();
    let (substituted_params, substituted_return) = infer_method_call_type_args(
        target,
        &sig,
        args,
        MethodInferenceOutputs {
            method_type_args: out_type_args,
            receiver_type_args: &mut receiver_args_inferred,
        },
        call_span,
        resolver.registry,
        diagnostics,
    );
    // Static dispatch's receiver.resolution starts as a bare leaf.
    // Stitch in the inferred type_args so IR lower can read them
    // (instance dispatch already carries the value's full type).
    if matches!(method_receiver, MethodReceiver::Static { .. })
        && !receiver_args_inferred.is_empty()
    {
        receiver.resolution = ResolvedType::Named {
            resolution: Resolution::Global(struct_id),
            type_args: receiver_args_inferred,
        };
    }
    // "Extend"-style domain check: a method registered at
    // `[receiver_head, method]` only applies to receivers whose
    // full `ResolvedType` matches the method's substituted `self`
    // type. Trait impls on concrete instantiations (e.g.
    // `impl Show for Bag<Int>`) lift `self` as `Bag<Int>`, so calls
    // on `Bag<String>` resolve the lookup but fail this check,
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
        return MethodCallOutcome::Method(ResolvedType::unresolved());
    }
    let substituted_explicit = method_receiver.explicit_params(&substituted_params);
    validate_arg_signature(
        args,
        substituted_explicit,
        &method_identifier,
        call_span,
        resolver,
        diagnostics,
    );
    MethodCallOutcome::Method(substituted_return)
}

/// Field-as-callable fallback for instance dispatch: if `struct_id`
/// has a field named `method` whose substituted type is
/// `fn (Ps...) -> R`, validate args against `Ps...` and return the
/// shape the caller stamps onto the AST. Any other field type (or
/// no field) bails to `None` so the caller emits the existing
/// "no method" diagnostic.
fn try_field_callable(
    struct_id: GlobalRegistryId,
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodCallOutcome> {
    let entry = resolver.registry.get(struct_id)?;
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        return None;
    };
    let (_, field) = definition.lookup_field(method)?;
    let receiver_args = match &receiver.resolution {
        ResolvedType::Named { type_args, .. } => type_args.clone(),
        _ => return None,
    };
    let subst = Substitution::from_args(struct_id, &receiver_args);
    let callee_ty = substitute(&field.ty, &subst);
    let ResolvedType::Anonymous(AnonymousKind::Function {
        params: fn_params,
        ret,
    }) = &callee_ty
    else {
        return None;
    };
    let expected = synthesize_local_call_params(fn_params);
    let callee_label = format!("{}.{method}", entry.identifier);
    resolve_args(args, Some(&expected), resolver, diagnostics);
    validate_local_call_signature(
        args,
        &expected,
        &callee_label,
        receiver.span,
        resolver,
        diagnostics,
    );
    let return_ty = (**ret).clone();
    Some(MethodCallOutcome::FieldCall {
        callee_ty,
        return_ty,
    })
}

/// Drive call-site type inference for a generic callee. Tries a
/// speculative pre-seed (`fill_from_expected` -> per-arg unify on a
/// scratch). On success the pre-seeded substitution wins so
/// `x: Int32 = identity(42)` keeps `T = Int32` via `literal_widens_into`.
/// On any conflict (e.g. outer expected `Unit` vs `identity(1) : Int`)
/// the fallback runs the original arg-first / advisory-fill order so
/// every existing diagnostic still fires.
fn infer_call_type_args(
    callee: Callee<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    site: CallSite<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let CallSite {
        out_type_args,
        expected,
        span: call_span,
    } = site;
    let mut subst = Substitution::single(callee.id, callee.type_params.len());
    if let Some(pre_seeded) = try_pre_seeded_subst(&callee, sig, args, expected, registry) {
        subst = pre_seeded;
    } else {
        let pairs = sig
            .params
            .iter()
            .zip(args.iter())
            .map(|(param, arg)| (&param.ty, &arg.value.resolution, arg.span));
        unify_pairs(pairs, &mut subst, registry, |conflict, arg_span| {
            emit_conflict(&callee, conflict, arg_span, registry, diagnostics);
        });
        if let Some(hint) = expected {
            fill_from_expected(&sig.return_type, hint, &mut subst, registry);
        }
    }
    finalize_inference(
        &[callee],
        &subst,
        &PhantomContext::Arguments,
        call_span,
        registry,
        diagnostics,
    );
    let substituted_params = sig
        .params
        .iter()
        .map(|p| ResolvedParam {
            name: p.name.clone(),
            ty: substitute(&p.ty, &subst),
        })
        .collect();
    let substituted_return = substitute(&sig.return_type, &subst);
    *out_type_args = subst.args(callee.id);
    (substituted_params, substituted_return)
}

/// Speculative pre-seed for [`infer_call_type_args`]: seed from
/// `expected`, run per-arg unify on a scratch, return `Some(subst)`
/// iff no conflict. Conflicts surface only from the fallback path.
fn try_pre_seeded_subst(
    callee: &Callee<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    expected: Option<&ResolvedType>,
    registry: &GlobalRegistry,
) -> Option<Substitution> {
    let hint = expected?;
    let mut scratch = Substitution::single(callee.id, callee.type_params.len());
    fill_from_expected(&sig.return_type, hint, &mut scratch, registry);
    let mut had_conflict = false;
    let pairs = sig
        .params
        .iter()
        .zip(args.iter())
        .map(|(param, arg)| (&param.ty, &arg.value.resolution, arg.span));
    unify_pairs(pairs, &mut scratch, registry, |_, _| {
        had_conflict = true;
    });
    (!had_conflict).then_some(scratch)
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

/// Resolve a bare call `name(...)`: prioritize the enclosing
/// scope, then fall through to the package scope. Inside a
/// struct/enum method, `Package.Enclosing.name` wins over
/// `Package.name` when both exist. The escape hatch for callers
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
    enclosing_type: Option<&[String]>,
    registry: &'a GlobalRegistry,
) -> Option<(GlobalRegistryId, &'a RegistryEntry)> {
    if let Some(enclosing) = enclosing_type {
        let mut scoped_path = enclosing.to_vec();
        scoped_path.push(name.to_string());
        if let Some(found) = registry.lookup(&Identifier::new(package, scoped_path)) {
            return Some(found);
        }
    }
    registry.lookup(&Identifier::new(package, vec![name.to_string()]))
}

/// Try to resolve `recv.method(args)` as a package-qualified
/// function call (`Pkg.f(args)`, e.g. `HTTP.get(url)`). Applies only
/// when the receiver is a bare identifier that names no local and no
/// type in scope, since locals and type receivers always win. Returns
/// `None` to fall through to method dispatch when the head doesn't
/// name a package with declarations, so existing diagnostics cover
/// unknown receivers.
fn try_package_function_call(
    receiver: &Expr,
    method: &str,
    args: &mut [Arg],
    site: CallSite<'_>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodCallOutcome> {
    let ExprKind::Ident { name, .. } = &receiver.kind else {
        return None;
    };
    if resolver.scope.lookup(name).is_some() {
        return None;
    }
    if lookup_type(std::slice::from_ref(name), resolver.resolution_scope()).is_some() {
        return None;
    }

    let call_span = site.span;
    let target = Identifier::new(name, vec![method.to_string()]);
    let Some((id, entry)) = resolver.registry.lookup(&target) else {
        resolver.registry.iter_in_package(name).next()?;
        resolve_args(args, None, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!("package `{name}` has no function `{method}`"),
            call_span,
        ));
        return Some(MethodCallOutcome::Method(ResolvedType::unresolved()));
    };
    let signature = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig.clone(),
        GlobalKind::Function(None) => panic!(
            "try_package_function_call: function `{}` has no lifted signature: \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        _ => return None,
    };
    let function = FunctionCallee {
        id,
        identifier: entry.identifier.clone(),
        signature,
        type_params: entry.type_params.clone(),
    };
    check_callee_visibility(entry, resolver, call_span, diagnostics);

    let return_ty = resolve_function_call(function, args, site, resolver, diagnostics);
    Some(MethodCallOutcome::PackageCall {
        fn_id: id,
        return_ty,
    })
}

/// Enforce the callee's [`VisibilityScope`] at the call site.
/// Mismatches push a diagnostic on `diagnostics`, but resolution still
/// proceeds so callers see exactly one error per offending call
/// site and downstream passes (seal, IR lower) walk a populated
/// tree.
fn check_callee_visibility(
    entry: &RegistryEntry,
    resolver: &Resolver<'_>,
    call_span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if callee_is_visible(
        entry.visibility,
        entry.identifier.package(),
        resolver.package,
        resolver.enclosing_type_id,
    ) {
        return;
    }
    match entry.visibility {
        VisibilityScope::Public => unreachable!("Public always passes callee_is_visible"),
        VisibilityScope::PackagePrivate => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "private function `{}` cannot be called from package `{}`",
                    entry.identifier, resolver.package,
                ),
                format!(
                    "`{}` is `priv fn`, callable only from package `{}` \
                     (declared at line {})",
                    entry.identifier,
                    entry.identifier.package(),
                    entry.span.start.line,
                ),
                call_span,
            ));
        }
        VisibilityScope::TypePrivate(owner) => {
            let owner_label = resolver
                .registry
                .get(owner)
                .map(|e| e.identifier.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "private method `{}` cannot be called from here",
                    entry.identifier,
                ),
                format!(
                    "`{}` is `priv fn`, callable only from methods on `{owner_label}` \
                     (declared at line {})",
                    entry.identifier, entry.span.start.line,
                ),
                call_span,
            ));
        }
    }
}

/// Pure visibility decision: does a callee with `scope` allow a
/// call from `caller_package` while resolving a method on
/// `caller_type_id`? `Public` is always reachable. `PackagePrivate`
/// requires `callee_package == caller_package`. `TypePrivate(owner)`
/// requires `caller_type_id == Some(owner)` (same-type, across all
/// inherent and protocol-impl blocks, since they share one id).
fn callee_is_visible(
    scope: VisibilityScope,
    callee_package: &str,
    caller_package: &str,
    caller_type_id: Option<GlobalRegistryId>,
) -> bool {
    match scope {
        VisibilityScope::Public => true,
        VisibilityScope::PackagePrivate => callee_package == caller_package,
        VisibilityScope::TypePrivate(owner) => caller_type_id == Some(owner),
    }
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
        let param = expected.and_then(|params| params.get(index));
        let expected_ty = param.map(|p| &p.ty);
        resolve_expr_with_expected(&mut arg.value, expected_ty, resolver, diagnostics);
    }
}

/// First-pass argument resolution for generic callees. Fully resolved
/// parameter hints flow into context-dependent expressions while
/// closure arguments wait for the second pass.
fn resolve_non_closure_args(
    args: &mut [Arg],
    expected: Option<&[ResolvedParam]>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, arg) in args.iter_mut().enumerate() {
        diagnose_named_arg(arg, diagnostics);
        if is_closure_expr(&arg.value.kind) {
            continue;
        }
        let expected_type = expected
            .and_then(|params| params.get(index))
            .map(|param| &param.ty)
            .filter(|ty| ty.is_resolved());
        resolve_expr_with_expected(&mut arg.value, expected_type, resolver, diagnostics);
    }
}

/// Second-pass arg resolution for generic callees: walk closure args
/// with the substituted param type as the expected hint so closure
/// param/return slots inherit any type-args inferred from the
/// non-closure args. Move marking for non-closure args happened in
/// the first pass. Closure args resolve to fresh function-pointer
/// rvalues (a `Copy` shape) so the second pass adds nothing here.
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
            format!("typecheck does not yet support named arguments (got `{name}`)",),
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

/// Substitute `subst` into every param's declared type. Used to
/// produce closure-arg expected types from a partial inference state.
fn substitute_params(params: &[ResolvedParam], subst: &Substitution) -> Vec<ResolvedParam> {
    params
        .iter()
        .map(|p| ResolvedParam {
            name: p.name.clone(),
            ty: substitute(&p.ty, subst),
        })
        .collect()
}

/// Check arg arity + per-position type compatibility. Diagnostics
/// use the callee's fully-qualified [`Identifier`]. Per-position
/// equivalence runs through [`check_compatible`] so a numeric
/// literal flowing into a narrow-int / narrow-float param coerces
/// when its compile-time value fits the param's range. The
/// resulting coercion stamps onto the arg's [`Expr::literal_coercion`]
/// for IR lower to consume.
fn validate_arg_signature(
    args: &mut [Arg],
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

    for (arg, param) in args.iter_mut().zip(expected_params.iter()) {
        let actual = arg.value.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible_stamping(&mut arg.value, &actual, &param.ty, resolver.registry) {
            None => {}
            Some(Mismatch::OutOfRange {
                rendered_value,
                width,
            }) => {
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
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee}` expects `{}`, got `{}`",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
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
fn synthesize_local_call_params(fn_params: &[ResolvedType]) -> Vec<ResolvedParam> {
    fn_params
        .iter()
        .enumerate()
        .map(|(index, ty)| ResolvedParam {
            name: format!("arg{index}"),
            ty: ty.clone(),
        })
        .collect()
}

/// Local-call counterpart to [`validate_arg_signature`]. Same
/// invariants (arity match + per-position type match plus the
/// literal-fit coercion fallback) but uses a bare `&str` callee
/// label (the local's surface name) so the diagnostic doesn't
/// fabricate a fully-qualified identifier the user never wrote.
fn validate_local_call_signature(
    args: &mut [Arg],
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
    for (arg, param) in args.iter_mut().zip(expected_params.iter()) {
        let actual = arg.value.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible_stamping(&mut arg.value, &actual, &param.ty, resolver.registry) {
            None => {}
            Some(Mismatch::OutOfRange {
                rendered_value,
                width,
            }) => {
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
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "argument `{}` to `{callee_label}` expects `{}`, got `{}`",
                        param.name,
                        display_resolution(&param.ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
                    ),
                    arg.span,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for [`callee_is_visible`], the pure half of the
    //! `priv fn` enforcement. Integration coverage lives in
    //! `tests/visibility.rs`. The cases here pin the decision matrix
    //! at the smallest possible API surface, including the
    //! cross-package `PackagePrivate` rejection path that surface
    //! syntax can't currently reach (`Pkg.fn(args)` doesn't resolve to
    //! top-level fns today).
    use super::callee_is_visible;
    use crate::registry::VisibilityScope;
    use koja_ast::identifier::GlobalRegistryId;

    #[test]
    fn public_is_always_visible() {
        let foo = GlobalRegistryId::new(7);
        assert!(callee_is_visible(VisibilityScope::Public, "A", "A", None));
        assert!(callee_is_visible(VisibilityScope::Public, "A", "B", None));
        assert!(callee_is_visible(
            VisibilityScope::Public,
            "A",
            "A",
            Some(foo)
        ));
    }

    #[test]
    fn package_private_matches_only_same_package() {
        let scope = VisibilityScope::PackagePrivate;
        assert!(callee_is_visible(scope, "Lib", "Lib", None));
        assert!(callee_is_visible(
            scope,
            "Lib",
            "Lib",
            Some(GlobalRegistryId::new(0))
        ));
        assert!(!callee_is_visible(scope, "Lib", "App", None));
        assert!(!callee_is_visible(
            scope,
            "Lib",
            "App",
            Some(GlobalRegistryId::new(0))
        ));
    }

    #[test]
    fn type_private_matches_only_same_owner() {
        let foo = GlobalRegistryId::new(3);
        let bar = GlobalRegistryId::new(4);
        let scope = VisibilityScope::TypePrivate(foo);

        assert!(callee_is_visible(scope, "A", "A", Some(foo)));
        // Cross-package same-owner is irrelevant: type-private is
        // anchored on identity, not package, but a type id is
        // unique across the program so this can't actually occur.
        assert!(callee_is_visible(scope, "A", "B", Some(foo)));

        assert!(!callee_is_visible(scope, "A", "A", Some(bar)));
        assert!(!callee_is_visible(scope, "A", "A", None));
    }
}
